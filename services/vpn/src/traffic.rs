use anyhow::{Context, Result, bail};
use std::{
    io::Write,
    process::{Command, Stdio},
};
use tidvar_payee::store::Peer;

/// Install the peer's two directional limits before adding its WireGuard key.
pub fn configure(peer: &Peer) -> Result<()> {
    let address = peer.address()?;
    let class = format!(
        "1:{:x}",
        peer.slot
            .checked_add(1)
            .context("Invalid VPN traffic class.")?
    );
    let batch = format!(
        "class replace dev {dev} parent 1:1 classid {class} htb rate 20mbit ceil 20mbit burst 128k cburst 128k quantum 1514\n\
         filter replace dev {dev} parent 1: protocol ip pref {slot} flower dst_ip {address}/32 classid {class}\n\
         filter replace dev {dev} parent ffff: protocol ip pref {slot} flower src_ip {address}/32 action police rate 20mbit burst 128k conform-exceed drop/ok\n",
        slot = peer.slot,
        dev = std::env::var("TIDVAR_VPN_INTERFACE").unwrap_or_else(|_| "tidvar".to_owned()),
    );
    let mut child = Command::new("/usr/sbin/tc")
        .args(["-batch", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("The VPN bandwidth limiter could not start.")?;
    child
        .stdin
        .take()
        .context("The VPN bandwidth limiter input was unavailable.")?
        .write_all(batch.as_bytes())?;
    if !child.wait_with_output()?.status.success() {
        bail!("The VPN bandwidth limit could not be installed. No connection was admitted.");
    }
    Ok(())
}
