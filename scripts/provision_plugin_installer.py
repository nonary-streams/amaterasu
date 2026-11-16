#!/usr/bin/env python3
"""Provision the optional machine driver broker during an authorized Linux install."""
import argparse, hashlib, json, os, pathlib, pwd, stat, subprocess

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--root',type=pathlib.Path,required=True)
    parser.add_argument('--host',type=pathlib.Path,required=True)
    parser.add_argument('--user',required=True)
    parser.add_argument('--publisher',action='append',required=True)
    args=parser.parse_args()
    if os.geteuid()!=0: parser.error('Run this provisioning step as an administrator.')
    root=args.root.resolve(strict=True); host=args.host.resolve(strict=True)
    if any(len(key)!=64 or any(c not in '0123456789abcdef' for c in key) for key in args.publisher): parser.error('Publisher fingerprints must be SHA-256.')
    if not host.is_file() or not root.is_dir(): parser.error('The installed host and runtime root must exist.')
    for path in [host, *host.parents]:
        metadata=path.stat()
        if metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) & 0o022:
            parser.error('Install the host executable in an administrator-owned, protected directory first.')
    user=pwd.getpwnam(args.user)
    if not user.pw_uid: parser.error('The runtime user must not be root.')
    config=pathlib.Path('/etc/amaterasu-plugin-installer'); state=pathlib.Path('/var/lib/amaterasu-plugin-installer')
    for directory in [config,state]:
        if directory.is_symlink(): parser.error('A service directory is a symbolic link.')
        directory.mkdir(mode=0o700,exist_ok=True); directory.chmod(0o700)
    policy={'root':str(root),'endpoint':'/run/amaterasu-plugin-installer/control.sock','approvals':str(state),'host_sha256':hashlib.sha256(host.read_bytes()).hexdigest(),'runtime_identity':str(user.pw_uid),'publishers':args.publisher}
    policy_path=config/'policy.json'
    if policy_path.is_symlink(): parser.error('The policy path is a symbolic link.')
    with policy_path.open('w',opener=lambda path,flags:os.open(path,flags|os.O_NOFOLLOW,0o600)) as stream: json.dump(policy,stream)
    policy_path.chmod(0o600)
    def quoted(value):
        value=str(value)
        if '\n' in value or '\x00' in value: parser.error('An installation path is invalid.')
        return '"'+value.replace('\\','\\\\').replace('"','\\"').replace('%','%%')+'"'
    unit='''[Unit]
Description=Amaterasu approved plugin driver installation
[Service]
Type=simple
User=root
RuntimeDirectory=amaterasu-plugin-installer
RuntimeDirectoryMode=0755
ExecStart={host} plugin-driver-service {policy}
TimeoutStopSec=30
KillMode=control-group
Restart=on-failure
[Install]
WantedBy=multi-user.target
'''.format(host=quoted(host),policy=quoted(policy_path))
    path=pathlib.Path('/etc/systemd/system/amaterasu-plugin-installer.service')
    if path.is_symlink(): parser.error('The service unit is a symbolic link.')
    path.write_text(unit);path.chmod(0o644)
    subprocess.run(['systemctl','daemon-reload'],check=True)
    subprocess.run(['systemctl','enable','--now','amaterasu-plugin-installer.service'],check=True)
    print('Plugin driver installation service enabled. No driver was installed.')
if __name__=='__main__': main()
