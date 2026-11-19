#!/usr/bin/env python3
"""Process-level discovery checks; no real Codex or mailbox traffic."""
import argparse,json,os,pathlib,shutil,subprocess,tempfile,time
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--boxen',required=True,type=pathlib.Path)
args=parser.parse_args();binary=args.boxen.resolve()
if os.name!='nt':
    with tempfile.TemporaryDirectory(prefix='boxen-codex-discovery-') as temporary:
        root=pathlib.Path(temporary); executable=root/'bin/codex';executable.parent.mkdir()
        executable.write_text('#!/bin/sh\n[ "$1" = --version ] || exit 88\nif [ -f "$0.slow" ]; then sleep 1.2; fi\necho codex-cli "$(cat "$0.version")"\n')
        executable.chmod(0o700);version=pathlib.Path(str(executable)+'.version');version.write_text('1.0.0')
        env={**os.environ,'HOME':str(root/'home'),'PATH':str(executable.parent)+':/usr/bin:/bin'}
        env.pop('BOXEN_CODEX',None)
        command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary),'codex-path']
        def resolve(*extra):
            result=subprocess.run(command+list(extra),env=env,capture_output=True,text=True,timeout=12)
            assert result.returncode==0,result.stderr
            return json.loads(result.stdout)
        assert pathlib.Path(resolve()['executable'])==executable
        pathlib.Path(str(executable)+'.slow').touch()
        recovered=resolve();assert recovered['version']=='codex-cli 1.0.0'
        assert 'slow retry' in recovered['probe_fallback']
        pathlib.Path(str(executable)+'.slow').unlink();version.write_text('2.0.0')
        assert resolve()['version']=='codex-cli 2.0.0'
        moved=root/'moved-cli';executable.rename(moved)
        assert not resolve('--codex',str(moved))['automatic']
        print(json.dumps({'ok':True,'cold_slow_probe_recovered':True,'upgrade_discovered':True,'pin_preserved':True,'workload_started':False}))
    raise SystemExit(0)
with tempfile.TemporaryDirectory(prefix='boxen-codex-discovery-') as temporary:
    root=pathlib.Path(temporary);desktop=root/'local/OpenAI/Codex/bin';desktop.mkdir(parents=True)
    logs=root/'logs';logs.mkdir()
    fixture=root/'fixture.exe'
    source=pathlib.Path(__file__).resolve().parents[1]/'addons/codex-desktop/tests/queue-fixture.rs'
    subprocess.run(['rustc',str(source),'-o',str(fixture)],check=True,timeout=60)
    env={**os.environ,'LOCALAPPDATA':str(root/'local'),'USERPROFILE':str(root/'home'),'BOXEN_SUPPORT_QUEUE_LOG':str(logs)}
    env.pop('BOXEN_CODEX',None)
    def install(name,version):
        folder=desktop/name;folder.mkdir();shutil.copy2(fixture,folder/'codex.exe');(folder/'version').write_text(version);return folder
    old=install('zzzz','0.166.0-alpha.9');new=install('aaaa','0.166.0-alpha.10');(desktop/'partial-update').mkdir()
    def resolve(*extra):
        process=subprocess.Popen([str(binary),'codex-path',*extra],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        stdout,stderr=process.communicate(timeout=8)
        assert process.returncode==0,stderr
        assert not list(pathlib.Path(tempfile.gettempdir()).glob(f'boxen-codex-version-{process.pid}-*.tmp')), 'probe capture leaked'
        return json.loads(stdout)
    assert pathlib.Path(resolve()['executable'])==new/'codex.exe'
    (new/'probe-mode').write_text('sleep');started=time.monotonic()
    assert pathlib.Path(resolve()['executable'])==old/'codex.exe'
    elapsed=time.monotonic()-started;assert .8<elapsed<4,elapsed
    (new/'probe-mode').write_text('flood');started=time.monotonic()
    assert pathlib.Path(resolve()['executable'])==old/'codex.exe'
    assert time.monotonic()-started<4
    (new/'probe-mode').unlink();(new/'version').write_text('0.167.0')
    selected=resolve();assert selected['version']=='codex-cli 0.167.0'
    (new/'codex.exe').unlink()
    assert pathlib.Path(resolve()['executable'])==old/'codex.exe'
    pin=resolve('--codex',str(new/'codex.exe'));assert not pin['automatic'] and pathlib.Path(pin['executable'])==new/'codex.exe'
    print(json.dumps({'ok':True,'numeric_prerelease':True,'partial_update_ignored':True,'probe_timeout_seconds':round(elapsed,3),'output_limit':True,'updated_version_picked_up':True,'disappeared_install_skipped':True,'pin_preserved':True}))
