import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import tempfile
import time

def command(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout.strip()

binary=Path(sys.argv[1]).resolve(strict=True)
name='wamn-native-b-auth-'+secrets.token_hex(6)
password=secrets.token_hex(24)
image=command(['docker','image','inspect','postgres:18','--format','{{.Id}}'])
container=None
with tempfile.TemporaryDirectory(prefix='wamn-native-b-auth-', dir='/tmp') as directory:
    env_file=Path(directory)/'postgres.env'
    env_file.write_text('POSTGRES_PASSWORD='+password+'\nPOSTGRES_DB=native_b\n')
    env_file.chmod(0o600)
    try:
        container=command(['docker','create','--pull=never','--name',name,'--label','wamn.proof.run='+name,'--env-file',str(env_file),'-p','127.0.0.1::5432',image])
        command(['docker','start',container])
        address=command(['docker','port',container,'5432/tcp'])
        assert re.fullmatch(r'127\.0\.0\.1:\d+',address), address
        port=address.split(':')[1]
        environment=os.environ.copy()
        environment['PGPASSWORD']=password
        environment['PGCONNECT_TIMEOUT']='1'
        for attempt in range(40):
            result=subprocess.run(['psql','-X','-w','-h','127.0.0.1','-p',port,'-U','postgres','-d','native_b','-Atqc','SELECT 1'],env=environment,capture_output=True,text=True)
            if result.returncode==0 and result.stdout.strip()=='1':
                break
            time.sleep(0.25)
        else:
            raise RuntimeError('owned PostgreSQL fixture did not become ready')
        environment.pop('PGPASSWORD')
        environment['WAMN_NATIVE_B_AUTH_PG_URL']=f'postgresql://postgres:{password}@{address}/native_b'
        test='router_driver::native_policy::tests::authenticated::native_authenticated_nested_authority_and_lifecycle'
        result=subprocess.run([str(binary),test,'--include-ignored','--exact','--nocapture','--test-threads=1'],env=environment,capture_output=True,text=True,timeout=45)
        output=result.stdout+result.stderr
        if password in output or re.search(r'(postgres(?:ql)?://[^\s]+:[^\s]+@|BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY)',output):
            raise RuntimeError('credential scan refused raw log publication')
        print(json.dumps({'container':container,'name':name,'image':image,'binary':str(binary),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'test':test}))
        print(output)
        assert result.returncode==0, f'test exited {result.returncode}'
        assert 'test result: ok. 1 passed; 0 failed; 0 ignored;' in output
    finally:
        if container:
            identity=command(['docker','inspect','--format','{{.Name}}|{{index .Config.Labels "wamn.proof.run"}}',container])
            assert identity=='/'+name+'|'+name, 'owned container identity changed'
            command(['docker','rm','--force','--volumes',container])
            print(json.dumps({'removed_owned_container':container}))
