#![forbid(unsafe_code)]
mod traffic;
use anyhow::{Context, Result, bail};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tidvar_core::params;
use tidvar_payee::{store::Store, usage::Limits};
use wireguard_control::{Backend, Device, DeviceUpdate, InterfaceName, Key, PeerConfigBuilder};
use zeroize::Zeroizing;

fn interface() -> String {
    std::env::var("TIDVAR_VPN_INTERFACE").unwrap_or_else(|_| "tidvar".to_owned())
}
fn port() -> u16 {
    std::env::var("TIDVAR_VPN_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(51820)
}
fn data() -> String {
    std::env::var("TIDVAR_DATA_DIR").unwrap_or_else(|_| "/root/tidvar-data".to_owned())
}
fn private() -> String {
    std::env::var("TIDVAR_VPN_KEY")
        .unwrap_or_else(|_| "/root/tidvar-private/keys/vpn-testnet.key".to_owned())
}
fn now() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn private_key() -> Result<Zeroizing<[u8; 32]>> {
    let private = private();
    let path = Path::new(&private);
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("The VPN key path must not be a link.");
    }
    let mut secret = Zeroizing::new([0; 32]);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            let mut generated = Key::generate_private();
            secret.copy_from_slice(&generated.0);
            generated.0.fill(0);
            file.write_all(secret.as_ref())?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut file = OpenOptions::new().read(true).open(path)?;
            if file.metadata()?.len() != 32 || file.metadata()?.permissions().mode() & 0o077 != 0 {
                bail!("The VPN key must be 32 bytes with private file permissions.");
            }
            file.read_exact(secret.as_mut())?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(secret)
}
fn device(name: &InterfaceName) -> Result<Device> {
    let mut device = Device::get(name, Backend::Kernel)
        .map_err(|_| anyhow::anyhow!("The VPN interface could not be read."))?;
    if let Some(mut secret) = device.private_key.take() {
        secret.0.fill(0);
    }
    Ok(device)
}
/// Cuts or restores a connected peer's internet without removing its tunnel, so the paying page
/// can still reach this service through it (to resume, pay or close) while WireGuard stays on.
fn block(address: std::net::Ipv4Addr, blocked: bool) -> Result<()> {
    let table = format!("{}_vpn", interface());
    let element = format!("{{ {address} }}");
    let status = Command::new("/usr/sbin/nft")
        .args([
            if blocked { "add" } else { "delete" },
            "element",
            "inet",
            &table,
            "blocked",
            &element,
        ])
        .status()
        .context("The VPN firewall could not be changed.")?;
    if !status.success() {
        bail!("The VPN firewall rejected a change.");
    }
    Ok(())
}
fn remove(name: &InterfaceName, key: [u8; 32]) -> Result<()> {
    DeviceUpdate::new()
        .remove_peer_by_key(&Key(key))
        .apply(name, Backend::Kernel)
        .map_err(|_| anyhow::anyhow!("A VPN connection could not be removed."))
}
fn stop() -> Result<()> {
    let name: InterfaceName = interface().parse()?;
    let Some(mut device) = Device::get(&name, Backend::Kernel).ok() else {
        return Ok(());
    };
    if let Some(mut secret) = device.private_key.take() {
        secret.0.fill(0);
    }
    if !Path::new(&private()).exists() {
        bail!("The VPN ownership key is missing; the interface was kept.");
    }
    let secret = private_key()?;
    let mut private = Key(*secret);
    let public = private.get_public();
    private.0.fill(0);
    if device.public_key.as_ref() != Some(&public) {
        bail!("The existing VPN belongs to another configuration.");
    }
    device
        .delete()
        .map_err(|_| anyhow::anyhow!("The VPN interface could not be removed."))
}
fn public_configuration(public: &Key) -> Result<()> {
    let output = Path::new(&data()).join("vpn.json");
    let temporary = Path::new(&data()).join("vpn.json.new");
    let value = serde_json::json!({"server_key":public.to_base64(),"endpoint":format!("38.49.216.59:{}", port())});
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(&value)?)?;
    file.sync_all()?;
    fs::rename(temporary, output)?;
    Ok(())
}
fn run() -> Result<()> {
    let limits = Limits::from_environment()?;
    let name: InterfaceName = interface().parse()?;
    let secret = private_key()?;
    let mut private = Key(*secret);
    let public = private.get_public();
    if let Ok(existing) = device(&name)
        && existing.public_key.as_ref() != Some(&public)
    {
        private.0.fill(0);
        bail!("The existing VPN belongs to another configuration.");
    }
    DeviceUpdate::new()
        .set_private_key(private.clone())
        .set_listen_port(port())
        .replace_peers()
        .apply(&name, Backend::Kernel)
        .map_err(|_| anyhow::anyhow!("The kernel VPN could not be configured."))?;
    private.0.fill(0);
    let network = Command::new("/bin/sh")
        .arg("/root/tidvar/deploy/vpn-network.sh")
        .output()
        .context("The VPN network setup could not start.")?;
    if !network.status.success() {
        bail!("The VPN network setup failed. No connection was admitted.");
    }
    let mut store = Store::open(&Path::new(&data()).join("payee.sqlite"))?;
    for peer in store.peers()? {
        store.recover_vpn(&peer.tab_id)?;
    }
    public_configuration(&public)?;
    if std::env::var_os("NOTIFY_SOCKET").is_some() {
        let status = Command::new("/usr/bin/systemd-notify")
            .arg("--ready")
            .status()?;
        if !status.success() {
            bail!("The VPN readiness signal could not be sent.");
        }
    }
    eprintln!("Tidvar VPN controller ready.");
    let mut shaped = HashSet::new();
    // Tabs that are closing with no peer installed need no more work until a restart.
    let mut finished: HashSet<[u8; 32]> = HashSet::new();
    // Connected peers whose internet is cut (paused or unpaid) but whose tunnel stays up.
    let mut blocked: HashSet<[u8; 32]> = HashSet::new();
    loop {
        let cycle = Instant::now();
        let registered = store.peers()?;
        if registered.len() > 50 {
            bail!("The VPN connection limit was exceeded.");
        }
        let observed = device(&name)?;
        let mut current: HashSet<[u8; 32]> = observed
            .peers
            .iter()
            .map(|peer| peer.config.public_key.0)
            .collect();
        let known: HashSet<[u8; 32]> = registered.iter().map(|peer| peer.public_key).collect();
        for key in current.difference(&known).copied().collect::<Vec<_>>() {
            remove(&name, key)?;
            current.remove(&key);
        }
        blocked.retain(|key| current.contains(key));
        let mut admitted = false;
        for peer in registered {
            if finished.contains(&peer.tab_id) && !current.contains(&peer.public_key) {
                continue;
            }
            let sampled = now()?;
            let usage = if let Some(actual) = observed
                .peers
                .iter()
                .find(|actual| actual.config.public_key.0 == peer.public_key)
            {
                store.record_transfer(
                    &peer.tab_id,
                    actual.stats.rx_bytes,
                    actual.stats.tx_bytes,
                    sampled,
                    limits,
                )?
            } else {
                store.usage(&peer.tab_id, sampled, limits)?
            };
            let mut balance = store.balance(&peer.tab_id, sampled)?;
            if let Some(limit) = usage.limit
                && !balance.paused
            {
                balance = store.pause_vpn(&peer.tab_id, sampled)?;
                eprintln!("VPN paused for {} at {}.", limit.code(), sampled);
            }
            // A closing tab keeps its tunnel, blocked, so the page can still finish the close
            // through it; the tunnel goes away when the service restarts.
            if balance.closing && !current.contains(&peer.public_key) {
                finished.insert(peer.tab_id);
                continue;
            }
            if balance.running && !current.contains(&peer.public_key) {
                store.recover_vpn(&peer.tab_id)?;
                continue;
            }
            let allowed = !balance.paused
                && !balance.closing
                && (balance.running || params::can_start_vpn(balance.paid, balance.used));
            // A tab that is not closing always has its tunnel installed; when it may not use the VPN
            // (paused or unpaid) the tunnel is blocked from the internet but still reaches us.
            if (allowed || !balance.closing)
                && !current.contains(&peer.public_key)
                && current.len() < 50
                && !admitted
            {
                let started = Instant::now();
                if !shaped.contains(&peer.slot) {
                    traffic::configure(&peer)?;
                    shaped.insert(peer.slot);
                }
                if !allowed {
                    // Blocked before the tunnel exists, so an unpaid peer never gets a moment online.
                    block(peer.address()?, true)?;
                    blocked.insert(peer.public_key);
                }
                store.reset_transfer_counter(&peer.tab_id, now()?)?;
                DeviceUpdate::new()
                    .add_peer(
                        PeerConfigBuilder::new(&Key(peer.public_key))
                            .replace_allowed_ips()
                            .add_allowed_ip(peer.address()?.into(), 32),
                    )
                    .apply(&name, Backend::Kernel)
                    .map_err(|_| anyhow::anyhow!("A VPN connection could not be admitted."))?;
                if allowed && let Err(error) = store.resume_vpn(&peer.tab_id, now()?) {
                    remove(&name, peer.public_key)?;
                    return Err(error.into());
                }
                current.insert(peer.public_key);
                admitted = true;
                eprintln!(
                    "VPN {} at {} in {}us.",
                    if allowed {
                        "connected"
                    } else {
                        "installed blocked"
                    },
                    now()?,
                    started.elapsed().as_micros()
                );
            } else if allowed && blocked.contains(&peer.public_key) {
                let started = Instant::now();
                block(peer.address()?, false)?;
                blocked.remove(&peer.public_key);
                store.resume_vpn(&peer.tab_id, now()?)?;
                eprintln!(
                    "VPN connected at {} in {}us.",
                    now()?,
                    started.elapsed().as_micros()
                );
            } else if !allowed
                && current.contains(&peer.public_key)
                && !blocked.contains(&peer.public_key)
            {
                let started = Instant::now();
                block(peer.address()?, true)?;
                blocked.insert(peer.public_key);
                eprintln!(
                    "VPN disconnected at {} in {}us.",
                    now()?,
                    started.elapsed().as_micros()
                );
            }
        }
        thread::sleep(Duration::from_millis(100).saturating_sub(cycle.elapsed()));
    }
}
fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => run(),
        Some("stop") => stop(),
        _ => bail!("Use tidvar-vpn or tidvar-vpn stop."),
    }
}
