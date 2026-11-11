#!/usr/bin/env python3
"""Install the repository's Rust toolchain into build/toolchain, without globals."""
import hashlib
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]
CHANNEL = tomllib.loads((ROOT / 'codex-rs/rust-toolchain.toml').read_text())['toolchain']['channel']
DOWNLOADS = ROOT / 'build/toolchain-downloads'
PREFIX = ROOT / 'build/toolchain'
DOWNLOADS.mkdir(parents=True, exist_ok=True)
manifest = DOWNLOADS / 'channel.toml'
subprocess.run(['curl', '--fail', '--location', '--silent', '--show-error', '--connect-timeout', '15',
                '--max-time', '120', 'https://static.rust-lang.org/dist/channel-rust-' + CHANNEL + '.toml',
                '--output', str(manifest)], check=True)
packages = tomllib.loads(manifest.read_text())['pkg']
for component in ['rustc', 'cargo', 'rust-std']:
    release = packages[component]['target']['x86_64-unknown-linux-gnu']
    url, expected = release['xz_url'], release['xz_hash']
    if not url.startswith('https://static.rust-lang.org/dist/'):
        raise ValueError('Unexpected toolchain origin')
    archive = DOWNLOADS / (component + '.tar.xz')
    if not archive.exists() or hashlib.file_digest(archive.open('rb'), 'sha256').hexdigest() != expected:
        subprocess.run(['curl', '--fail', '--location', '--silent', '--show-error', '--connect-timeout', '15',
                        '--max-time', '600', url, '--output', str(archive)], check=True)
    with archive.open('rb') as data:
        if hashlib.file_digest(data, 'sha256').hexdigest() != expected:
            raise ValueError('Toolchain archive checksum mismatch: ' + component)
    subprocess.run(['tar', '--extract', '--file', str(archive), '--directory', str(DOWNLOADS),
                    '--no-same-owner', '--no-same-permissions'], check=True)
    extracted = DOWNLOADS / Path(url).name.removesuffix('.tar.xz')
    subprocess.run([str(extracted / 'install.sh'), '--prefix=' + str(PREFIX), '--disable-ldconfig'], check=True)
for binary in ['cargo', 'rustc']:
    subprocess.run([str(PREFIX / 'bin' / binary), '--version'], check=True)
