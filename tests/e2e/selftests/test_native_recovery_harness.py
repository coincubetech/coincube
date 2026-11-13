"""Offline native recovery guards. No live fixture or GUI acceptance claimed."""
from copy import deepcopy
import io
import json
from pathlib import Path
from types import SimpleNamespace
import struct
import sys

import pytest
from harness import native_recovery as native, native_recovery_verify as public
from harness.provisioning import FreshRunner
from harness.state import Blocked, Failed


@pytest.fixture
def config(tmp_path):
    return {'version': 1, 'checkout': str(tmp_path/'checkout'), 'revision': 'current'}


@pytest.mark.parametrize('field', ['binary', 'receipt_file', 'seed', 'pin', 'factor', 'original_profile', 'driver'])
def test_external_secret_binary_or_receipt_injection_is_not_a_native_config(config, field):
    config[field] = '/untrusted/input'
    with pytest.raises(Failed): native.options(config)


def test_missing_prerequisite_blocks_before_credentials_or_run_creation(config, tmp_path, monkeypatch):
    def output(argv, **kw):
        if 'get-url' in argv: return b'git@github.com:coincubetech/coincube.git\n'
        if 'status' in argv: return b''
        return b'a'*40+b'\n'
    monkeypatch.setattr(native.subprocess, 'check_output', output)
    monkeypatch.setattr(native.subprocess, 'run', lambda *a, **k: SimpleNamespace(returncode=1))
    with pytest.raises(Blocked, match='prerequisite'): native.NativeRecovery(config, tmp_path/'run', {})
    assert not (tmp_path/'run').exists()


def test_missing_verifier_dependency_blocks_before_helper_or_credentials(config, tmp_path, monkeypatch):
    monkeypatch.setattr(native, 'source', lambda _: {'head': 'a'*40})
    def missing(): raise Blocked('NATIVE_RECOVERY_VERIFY_DEPENDENCIES', 'missing')
    monkeypatch.setattr(native, 'dependencies', missing)
    with pytest.raises(Blocked, match='missing'): native.NativeRecovery(config, tmp_path/'run', {})
    assert not (tmp_path/'run').exists()


def test_environment_preflight_blocks_before_mobile_or_native_fixtures(config, monkeypatch):
    calls = []
    runner = FreshRunner.__new__(FreshRunner); runner.config = {'native_recovery': config}
    runner.core = SimpleNamespace(chain=lambda: calls.append('mobile'))
    def missing(_): raise Blocked('NATIVE_RECOVERY_PR1174', 'missing prerequisite')
    monkeypatch.setattr(native, 'preflight', missing)
    with pytest.raises(Blocked): runner.stage_environment()
    assert calls == []


def test_funding_gate_runs_before_saved_funding_or_sponsor_faucet(config, monkeypatch):
    runner = FreshRunner.__new__(FreshRunner); runner.config = {'native_recovery': config}
    def missing(_): raise Blocked('NATIVE_RECOVERY_PR1174', 'missing prerequisite')
    monkeypatch.setattr(native, 'gate', missing)
    # No artifacts/provision/phone/funding client installed: any use before the
    # native gate would raise instead of the required prerequisite block.
    with pytest.raises(Blocked): runner.stage_funding()


def test_old_config_is_explicitly_legacy_mobile_only():
    evidence = {}
    runner = SimpleNamespace(config={}, journal=SimpleNamespace(artifact=lambda k,v:evidence.update({k:v})))
    native.gate(runner)
    assert evidence == {'recovery_coverage': 'legacy_mobile_only'}


def fixture(chain):
    public.dependencies()
    from bip32 import BIP32
    from bip380.descriptors import Descriptor
    from coincurve import PrivateKey
    from test_framework.serializations import CTransaction, CTxIn, CTxOut, COutPoint, PSBT, PSBTMap, hash160, sighash_all_witness
    from test_btcb2_unified_sighash import unified_sighash
    root = BIP32.from_seed(b'\x17'*32, network='test')
    fp = hash160(root.get_pubkey_from_path([]))[:4].hex()
    path = [0x80000030,0x80000001,0x80000000,0x80000002]
    xpub = root.get_xpub_from_path(path)
    descriptor = f'wsh(or_d(pk([{fp}/48h/1h/0h/2h]{xpub}/<0;1>/*),and_v(v:pk([{fp}/48h/1h/0h/2h]{xpub}/<2;3>/*),older(144))))'
    receive, change = Descriptor.from_str(descriptor).singlepath_descriptors(); receive.derive(0); change.derive(0)
    parent = CTransaction();parent.vin=[CTxIn(COutPoint(0,0xffffffff))];parent.vout=[CTxOut(100000,bytes(receive.script_pubkey))]
    tx=CTransaction();tx.vin=[CTxIn(COutPoint(int(parent.txid().hex(),16),0))];tx.vout=[CTxOut(99738,bytes(change.script_pubkey))]
    origins={root.get_pubkey_from_path(path+[branch,0]):bytes.fromhex(fp)+b''.join(struct.pack('<I',v) for v in path+[branch,0]) for branch in (0,2)}
    psbt=PSBT(g=PSBTMap({0:tx.serialize()}),i=[PSBTMap({0:parent.serialize(),1:parent.vout[0].serialize(),5:bytes(receive.script_sighash),6:origins})],o=[PSBTMap()]);psbt.tx=tx
    message=unified_sighash(tx,0,0x21,1,parent.vout,bytes(receive.script_sighash)) if chain==public.CHAINS[1] else sighash_all_witness(bytes(receive.script_sighash),psbt,0)
    signatures={root.get_pubkey_from_path(path+[branch,0]):PrivateKey(root.get_privkey_from_path(path+[branch,0])).sign(message,hasher=None)+bytes([0x21 if chain==public.CHAINS[1] else 1]) for branch in (0,2)}
    psbt.i[0].map[0xfc if chain==public.CHAINS[1] else 2] = {b'\x08coincube\x00'+k:v for k,v in signatures.items()} if chain==public.CHAINS[1] else signatures
    identity={'cube':{'network':chain},'descriptor':descriptor,'fingerprint':fp}
    return psbt,identity


@pytest.mark.parametrize('chain', public.CHAINS)
def test_independent_unfunded_verifier_requires_exact_primary_and_recovery_signatures(chain):
    psbt,identity=fixture(chain);proof=public.verify(psbt.serialize(),identity,chain)
    assert proof['verified_key_count']==2 and proof['verified_paths']==[[0,0],[2,0]]
    assert proof['sighash']==(1 if chain==public.CHAINS[0] else 0x21)
    assert proof['funded'] is proof['recognized_utxo'] is proof['gui'] is proof['broadcast'] is False


@pytest.mark.parametrize('chain',public.CHAINS)
@pytest.mark.parametrize('change',['chain','fingerprint','path','pubkey','output','utxo','drop_signature','wrong_storage','alter_signature'])
def test_independent_verifier_refuses_bad_domain_origins_outputs_and_signature_sets(chain,change):
    psbt,identity=fixture(chain)
    field=0xfc if chain==public.CHAINS[1] else 2
    if change=='chain': identity['cube']['network']=public.CHAINS[1] if chain==public.CHAINS[0] else public.CHAINS[0]
    elif change=='fingerprint': identity['fingerprint']='00000000'
    elif change=='path':
        key=next(iter(psbt.i[0].map[6]));psbt.i[0].map[6][key]=psbt.i[0].map[6][key][:-4]+struct.pack('<I',99)
    elif change=='pubkey':
        key=next(iter(psbt.i[0].map[6]));psbt.i[0].map[6][b'\x02'+b'\x01'*32]=psbt.i[0].map[6].pop(key)
    elif change=='output': psbt.tx.vout[0].nValue-=1;psbt.g.map[0]=psbt.tx.serialize()
    elif change=='utxo': psbt.i[0].map[1]=psbt.i[0].map[1]+b'\0'
    elif change=='drop_signature': psbt.i[0].map[field].pop(next(iter(psbt.i[0].map[field])))
    elif change=='wrong_storage':
        sigs=psbt.i[0].map.pop(field);psbt.i[0].map[2 if field==0xfc else 0xfc]={k[10:] if field==0xfc else b'\x08coincube\x00'+k:v for k,v in sigs.items()}
    else:
        key=next(iter(psbt.i[0].map[field]));value=psbt.i[0].map[field][key];psbt.i[0].map[field][key]=value[:-2]+bytes([value[-2]^1])+value[-1:]
    with pytest.raises(Failed): public.verify(psbt.serialize(),identity,chain)


@pytest.mark.parametrize('raw',[b'psbt\xff\x01\x00\x01x\x01\x00\x01y\x00', b'psbt\xff\xfd\x01\x00\x00\x01x\x00'])
def test_noncanonical_or_duplicate_psbt_maps_are_refused(raw):
    with pytest.raises(Failed): public.canonical_maps(raw)


def offline_protocol(config, tmp_path, monkeypatch, crash_retirement=False):
    """Unit-only fake worker; never injectable through a live CLI/config."""
    import hashlib
    from harness.state import atomic_write
    binding={'head':'a'*40,'tree':'b'*40,'prerequisite':native.PREREQUISITE}
    executable=tmp_path/'test-only-binary';executable.write_bytes(b'not an executable')
    receipt={'source':binding,'sha256':hashlib.sha256(executable.read_bytes()).hexdigest()}
    monkeypatch.setattr(native,'preflight',lambda _:binding)
    monkeypatch.setattr(native,'source',lambda _:binding)
    monkeypatch.setattr(native,'build',lambda *args:(executable,receipt))
    monkeypatch.setattr(native,'signed_identity',lambda *args:None)
    calls=[]; interrupted=[False]
    def worker(executable,request,directory,access_token=None):
        action=request['action'];cfg=request['config'];calls.append((cfg['chain'],action))
        case=Path(cfg['case_root']);state_path=case/'state.json'
        pid={'preflight':10,'bind_auth':11,'create_export':100,'wipe_original':150,'restore':200,'reopen_sign':300,'retire_remote':400}[action]
        record={'pid':pid,'start_id':str(pid),'exited':True,'returncode':0}
        if action in ('preflight','bind_auth'):return record
        if action=='create_export':
            case.mkdir(mode=0o700);(case/'original').mkdir(mode=0o700)
            psbt,identity=fixture(cfg['chain'])
            state={'config':cfg,'pending':None,'stage':'exported','creator':{'pid':100,'start_id':'100'},'restorer':None,
                   'identity':identity,'original_factor':'old','restored_factor':None,'proof_sha256':None}
        else:state=json.loads(public_state_read(state_path))
        if action=='wipe_original':(case/'original').rename(case/'quarantined-original');state['stage']='wiped'
        if action=='restore':
            (case/'restored').mkdir(mode=0o700);state.update(stage='restored',restorer={'pid':200,'start_id':'200'},restored_factor='new')
        if action=='reopen_sign':
            psbt,identity=fixture(cfg['chain']);raw=psbt.serialize();atomic_write(case/'unfunded-key-control.psbt',raw)
            state.update(stage='signed_unfunded',proof_sha256=hashlib.sha256(raw).hexdigest())
        if action=='retire_remote':state['stage']='retired_unfunded'
        atomic_write(state_path,json.dumps(state).encode())
        if action=='retire_remote' and crash_retirement and not interrupted[0]:
            interrupted[0]=True;raise Blocked('TEST_LOST_RETIREMENT_RESPONSE','unit-only interruption')
        return record
    monkeypatch.setattr(native,'process',worker)
    return calls


def public_state_read(path):
    from harness.state import private_read
    return private_read(path)


def test_offline_orchestration_orders_both_chains_and_retirement_without_gui_claim(config,tmp_path,monkeypatch):
    calls=offline_protocol(config,tmp_path,monkeypatch)
    suite=native.NativeRecovery(config,tmp_path/'run',{'consumer':'offline-only-test'})
    try:
        result=suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1)
        assert list(result['chains'])==list(public.CHAINS)
        assert result['gui'] is result['funded'] is result['broadcast'] is False
        assert all(result['chains'][c]['verified_key_count']==2 for c in public.CHAINS)
        assert calls==[(c,a) for c in public.CHAINS for a in ('preflight','bind_auth',*native.ACTIONS,'retire_remote')]
        before=list(calls);suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1)
        assert calls==before  # Same consumer resumes retained proof, not a new run.
    finally:suite.close()


def test_lost_retirement_response_reconciles_only_exact_completed_fixture(config,tmp_path,monkeypatch):
    calls=offline_protocol(config,tmp_path,monkeypatch,crash_retirement=True)
    suite=native.NativeRecovery(config,tmp_path/'run',{'consumer':'offline-only-test'})
    try:
        with pytest.raises(Blocked,match='unit-only'):suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1)
        result=suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1)
        assert result['remote_retirement']=='verified_native_fixture_only'
        assert calls.count((public.CHAINS[0],'create_export'))==1
        assert calls.count((public.CHAINS[0],'retire_remote'))==2
    finally:suite.close()


def test_completed_consumer_refuses_replaced_retained_state(config,tmp_path,monkeypatch):
    calls=offline_protocol(config,tmp_path,monkeypatch)
    suite=native.NativeRecovery(config,tmp_path/'run',{'consumer':'offline-only-test'})
    try:
        suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1)
        case=next(p for p in suite.directory.iterdir() if p.is_dir())
        from harness.state import atomic_write
        value=json.loads(public_state_read(case/'state.json'));value['untrusted_extra']='replaced'
        atomic_write(case/'state.json',json.dumps(value).encode());before=list(calls)
        with pytest.raises(Failed,match='RETAINED_STATE_CHANGED'):suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1)
        assert calls==before
    finally:suite.close()


def test_replaced_helper_during_public_verification_never_reaches_retirement(config,tmp_path,monkeypatch):
    calls=offline_protocol(config,tmp_path,monkeypatch)
    original=native.verify
    def replace(raw,identity,chain):
        result=original(raw,identity,chain)
        (tmp_path/'test-only-binary').write_bytes(b'replaced after verified signing')
        return result
    monkeypatch.setattr(native,'verify',replace)
    suite=native.NativeRecovery(config,tmp_path/'run',{'consumer':'offline-only-test'})
    try:
        with pytest.raises(Failed,match='BUILD_CHANGED'):suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1)
        assert not any(action=='retire_remote' for _,action in calls)
    finally:suite.close()


def test_checked_process_revalidates_source_hash_and_identity_before_and_after(config,tmp_path,monkeypatch):
    executable=tmp_path/'fixture';executable.write_bytes(b'owned binary')
    import hashlib
    binding={'head':'a'*40};receipt={'source':binding,'sha256':hashlib.sha256(executable.read_bytes()).hexdigest()}
    checks=[]
    monkeypatch.setattr(native,'source',lambda _:checks.append('source') or binding)
    monkeypatch.setattr(native,'signed_identity',lambda *args:checks.append('identity'))
    monkeypatch.setattr(native,'process',lambda *args,**kwargs:checks.append('process') or {})
    native.checked_process(config,binding,executable,receipt,{},tmp_path)
    assert checks==['source','identity','process','source','identity']
    def changed(*args,**kwargs):executable.write_bytes(b'replaced during process');return {}
    monkeypatch.setattr(native,'process',changed)
    with pytest.raises(Failed,match='BUILD_CHANGED'):native.checked_process(config,binding,executable,receipt,{},tmp_path)


def test_completed_resume_revalidates_signer_identity_instead_of_returning_cached_acceptance(config,tmp_path,monkeypatch):
    calls=offline_protocol(config,tmp_path,monkeypatch)
    suite=native.NativeRecovery(config,tmp_path/'run',{'consumer':'offline-only-test'})
    try:
        suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1);before=list(calls)
        def rejected(*args):raise Failed('NATIVE_RECOVERY_HELPER_IDENTITY','identity changed')
        monkeypatch.setattr(native,'signed_identity',rejected)
        with pytest.raises(Failed,match='identity changed'):suite.execute({'tokens':{'access_token':'unit-not-a-credential'}},1)
        assert calls==before
    finally:suite.close()
