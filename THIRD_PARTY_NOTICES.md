# Third-party notices

## tlock IBE implementation

The quicknet encryption in `crates/tidvar-seal/src/ibe.rs` is adapted from
`tlock` 0.0.10, distributed under the MIT License.

Copyright for portions of project thibmeu/tlock-js are held by 2022 Timofey Luin
as part of project timoftime/tlock-rs. All other copyright for project
thibmeu/tlock-rs are held by 2023 Thibault Meunier.

Copyright (c) 2023 Thibault Meunier

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## English backup words

The encoded English word list in `crates/tidvar-payer/src/english.hex` is the
BIP-39 English word list from the Bitcoin BIPs repository:
https://github.com/bitcoin/bips/blob/master/bip-0039/english.txt

The decoded UTF-8 file has SHA-256
`2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda`.
BIP-39 is MIT-licensed and authored by Marek Palatinus, Pavol Rusnak,
Aaron Voisine and Sean Bowe. Tidvar implements the 256-bit entropy and checksum
encoding itself; the backup root is derived by Tidvar's own domain-separated
hash, rather than BIP-39's password-based wallet seed derivation.
