import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile

root=Path.cwd()
script=root/'docs/perf/2026.09/receiving-postcommit/tools/mutation.py'
namespace={'__name__':'mutation_offline_checks','__file__':str(script)}
exec(compile(script.read_bytes(),str(script),'exec'),namespace)
namespace['git']=lambda tree,*arguments: 'f'*40 if arguments[:2] == ('rev-parse','HEAD') else ''
checks=[]

def refuses(operation, phrase):
    try:
        operation()
    except RuntimeError as error:
        assert phrase in str(error), str(error)
        return
    raise AssertionError('expected refusal: '+phrase)

with tempfile.TemporaryDirectory(prefix='wamn-postcommit-mutation-check-') as temporary:
    temporary=Path(temporary)
    for control in ('replay-reset','timeout-terminal'):
        tree=temporary/control
        tree.mkdir()
        originals={}
        metadata={}
        for index,relative in enumerate(namespace['paths_for'](control)):
            path=tree/relative
            path.parent.mkdir(parents=True,exist_ok=True)
            data=(root/relative).read_bytes()
            path.write_bytes(data)
            mode=0o640 if index == 0 else 0o644
            path.chmod(mode)
            times=(1700000000123456789+index,1700000100987654321+index)
            os.utime(path,ns=times)
            originals[relative]=data
            metadata[relative]={'mode':mode,'atime_ns':times[0],'mtime_ns':times[1]}
        evidence=temporary/(control+'-evidence')
        state=namespace['prepare'](tree,evidence,control)
        assert state['status']=='prepared'
        for relative,expected in metadata.items():
            assert all(state['files'][relative]['before'][key] == value for key,value in expected.items())
            assert (evidence/'backups'/relative).read_bytes() == originals[relative]
        prepared={relative:(tree/relative).read_bytes() for relative in state['mutated_paths']}
        if control == 'replay-reset':
            assert prepared[namespace['REPLAY_SQL']] == namespace['SQL_AFTER']
            changed=json.loads(prepared[namespace['MANIFEST']])
            original=json.loads(originals[namespace['MANIFEST']])
            original['custom_operations']['quality.create_inspection']['relations'][0]['update_fields']=['status','row_version']
            assert changed == original
            for relative in namespace['GENERATED']:
                (tree/relative).write_bytes(originals[relative]+b'\n')
            refuses(lambda:namespace['restore'](tree,evidence,state),'unexpected bytes or mode')
            namespace['seal_generated'](tree,evidence,state)
            assert state['status']=='generated-sealed'
            refuses(lambda:namespace['seal_generated'](tree,evidence,state),'Seal generated files once')
            checks.append('replay generation requires sealing and refuses a second seal')
        else:
            assert prepared[namespace['TIMEOUT_RUST']].count(namespace['TIMEOUT_AFTER']) == 1
            refuses(lambda:namespace['seal_generated'](tree,evidence,state),'Seal generated files once')
        relative=state['mutated_paths'][0]
        path=tree/relative
        path.write_bytes(prepared[relative]+b'\nunexpected edit\n')
        refuses(lambda:namespace['restore'](tree,evidence,state),'unexpected bytes or mode')
        assert path.read_bytes().endswith(b'unexpected edit\n')
        path.write_bytes(prepared[relative])
        saved_mode=path.stat().st_mode & 0o777
        path.chmod(saved_mode ^ 0o010)
        refuses(lambda:namespace['restore'](tree,evidence,state),'unexpected bytes or mode')
        path.chmod(saved_mode)
        state=namespace['load'](tree,evidence)
        backup=evidence/'backups'/relative
        backup.write_bytes(b'corrupt backup')
        refuses(lambda:namespace['load'](tree,evidence),'Backup hash differs')
        backup.write_bytes(originals[relative])
        state=namespace['load'](tree,evidence)
        namespace['restore'](tree,evidence,state)
        for relative,data in originals.items():
            path=tree/relative
            actual=path.stat()
            expected=metadata[relative]
            assert stat.S_IMODE(actual.st_mode)==expected['mode']
            assert actual.st_atime_ns==expected['atime_ns']
            assert actual.st_mtime_ns==expected['mtime_ns']
            assert path.read_bytes()==data
        namespace['restore'](tree,evidence,namespace['load'](tree,evidence))
        assert json.loads((evidence/'state.json').read_text())['status']=='restored'
        checks.append(control+': exact mutation, backup guard, unexpected-byte/mode guard, exact byte/mode/atime/mtime restoration, repeat restoration')
    source={namespace['TIMEOUT_RUST']:(root/namespace['TIMEOUT_RUST']).read_bytes()}
    source[namespace['TIMEOUT_RUST']]+=b'\n'+namespace['TIMEOUT_BEFORE']+b'\n'
    refuses(lambda:namespace['mutations']('timeout-terminal',source),'exactly one original match')
    source[namespace['TIMEOUT_RUST']]=b'no expected mapping'
    refuses(lambda:namespace['mutations']('timeout-terminal',source),'exactly one original match')
    checks.append('zero and repeated timeout mutation matches refuse')

result={'schema':'receiving-postcommit-mutation-offline-checks/v1','result':'pass',
        'scope':'temporary ordinary directories only, no worktrees, builds, services, or live gates',
        'source_sha256':hashlib.sha256(script.read_bytes()).hexdigest(),'checks':checks}
evidence=root/'docs/perf/2026.09/receiving-postcommit/mutation-tool-checks-001'
evidence.mkdir()
(evidence/'result.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result,sort_keys=True))
