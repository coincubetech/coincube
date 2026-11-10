"""Recovery and credential replay checks without a real phone or Keychain."""
from copy import deepcopy
from types import SimpleNamespace
from xml.sax.saxutils import escape

import pytest

from harness.onboarding import Onboarding, recovery_words
from harness.secrets import SecretStore
from harness.state import Blocked, Failed

WORDS = ['abandon', 'ability', 'able', 'about', 'above', 'absent', 'absorb', 'abstract', 'absurd', 'abuse', 'access', 'accident']


def xml(labels):
    return '<root>' + ''.join('<node type="XCUIElementTypeStaticText" visible="true" label="' + escape(v, {'"': '&quot;'}) + '"/>' for v in labels) + '</root>'


class Store:
    def __init__(self, value=None):
        self.value = deepcopy(value)
        self.writes = []

    def load(self):
        return deepcopy(self.value)

    def save(self, value):
        self.value = deepcopy(value)
        self.writes.append(deepcopy(value))


def controller(store, labels=None):
    phone = SimpleNamespace(device_digest='device', labels=lambda: labels or set(),
                            restore_navigation=lambda: None)
    return Onboarding(phone, store, 'account', .1)


def test_recovery_column_order_uses_explicit_indices():
    indices = [1, 7, 2, 8, 3, 9, 4, 10, 5, 11, 6, 12]
    assert recovery_words(xml([v for i in indices for v in (str(i), WORDS[i-1])])) == WORDS
    assert recovery_words(xml([f'{i}. {WORDS[i-1]}' for i in reversed(indices)])) == WORDS


def test_partial_recovery_is_never_accepted():
    with pytest.raises(Blocked) as error:
        recovery_words(xml([f'{i+1} {v}' for i, v in enumerate(WORDS[:-1])]))
    assert error.value.code == 'RECOVERY_WORDS'


def test_conflicting_recovery_indices_fail():
    with pytest.raises(Failed) as error:
        recovery_words(xml(['1 abandon', '1 ability']))
    assert error.value.code == 'RECOVERY_INDEX'


def test_existing_unowned_master_is_never_replaced():
    store = Store()
    flow = controller(store, {'Home', 'Cubes', 'Settings'})
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'PHONE_MASTER_NOT_OWNED'
    assert store.writes == []


def test_credential_identity_cannot_change():
    flow = controller(Store({'identity': 'foreign', 'verified': True, 'pin': '123456'}))
    with pytest.raises(Failed) as error:
        flow.setup()
    assert error.value.code == 'PHONE_IDENTITY'


def test_pending_pin_attempt_survives_new_controller_and_new_run():
    store = Store({'identity': 'account:device', 'verified': True, 'pin': '123456'})
    flow = controller(store)
    assert flow.pin_attempt('key:run-one') == '123456'
    resumed = controller(store)
    for purpose in ('key:run-one', 'key:run-two', 'sign:different-run'):
        with pytest.raises(Blocked) as error:
            resumed.pin_attempt(purpose)
        assert error.value.code == 'PHONE_PIN_UNCERTAIN'
    resumed.reconcile_pin('unrelated-proof')
    assert store.value['pending_attempt'] == 'key:run-one'
    resumed.reconcile_pin('key:run-one')
    assert resumed.pin_attempt('sign:run-one') == '123456'


def test_initial_setup_persists_pin_and_all_recovery_words_before_using_them(monkeypatch):
    monkeypatch.setattr('harness.onboarding.secrets.randbelow', lambda _: 123456)
    store = Store()
    view = ['Use a PIN']
    digits = []
    fields = []
    for _ in range(3):
        field = SimpleNamespace(clear=lambda: None, text=None)
        field.send_keys = lambda value, field=field: setattr(field, 'text', value)
        fields.append(field)
    source = SimpleNamespace(page_source='')
    def click(label):
        if label == 'Use a PIN':
            assert store.value['pin'] == '123456'
            view[0] = 'Create a PIN'
        elif label.isdigit():
            assert store.value['entered_' + view[0]] is True
            digits.append(label)
            if len(digits) % 6 == 0:
                view[0] = 'Confirm your PIN' if view[0] == 'Create a PIN' else 'Write down these 12 words'
                source.page_source = xml([f'{i+1} {v}' for i, v in enumerate(WORDS)])
        elif label == 'I have written them down':
            assert store.value['words'] == WORDS
            view[0] = 'Check what you wrote down'
            source.page_source = xml(['Word 2', 'Word 7', 'Word 11'])
        elif label == 'Confirm and finish setup':
            assert [f.text for f in fields] == [WORDS[1], WORDS[6], WORDS[10]]
            view[0] = 'Go to home'
        elif label == 'Go to home':
            assert store.value['verified'] is True
            view[0] = 'Home'
        else:
            pytest.fail('Unexpected click')
    phone = SimpleNamespace(device_digest='device', labels=lambda: set(view), click=click, driver=source)
    flow = Onboarding(phone, store, 'account', 1)
    flow.fields = lambda: fields
    flow.reveal = lambda _: None
    assert flow.setup() == '123456'
    assert ''.join(digits) == '123456123456'
    assert store.value['words'] == WORDS
    assert store.value['verified'] is True


def test_interrupted_pin_setup_does_not_repeat_partial_keypad_entry():
    store = Store({'identity': 'account:device', 'pin': '123456', 'verified': False, 'entered_Create a PIN': True})
    flow = controller(store, {'Create a PIN'})
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'SETUP_PIN_UNCERTAIN'


def test_encrypted_secret_store_uses_cas_and_verifies_readback():
    record = {'value': None, 'sha256': None}
    calls = []
    def call(action, **fields):
        calls.append((action, fields))
        if action == 'secret_put':
            assert fields['expected_previous_sha256'] == record['sha256']
            record.update(value=fields['secret_value'], sha256='a'*64)
            return {'sha256': record['sha256']}
        return deepcopy(record)
    store = SecretStore(SimpleNamespace(call=call))
    assert store.load() is None
    store.save({'pin': '123456'})
    assert calls[-1][0] == 'secret_get'
    store.save({'pin': '123456', 'verified': True})
    assert calls[-2][1]['expected_previous_sha256'] == 'a'*64


def test_encrypted_secret_store_rejects_changed_readback():
    store = SecretStore(SimpleNamespace(call=lambda action, **kwargs: (
        {'sha256': 'a'*64} if action == 'secret_put' else {'sha256': 'a'*64, 'value': {'wrong': True}})))
    with pytest.raises(Failed) as error:
        store.save({'pin': '123456'})
    assert error.value.code == 'SECRET_READBACK'


def test_setup_success_auto_route_reconciles_only_saved_recovery_submission():
    store = Store({'identity': 'account:device', 'pin': '123456', 'verified': False,
                   'words': WORDS, 'entered_Create a PIN': True, 'entered_Confirm your PIN': True,
                   'recovery_submission_started': True})
    flow = controller(store, {'Home', 'Cubes', 'Settings'})
    assert flow.setup() == '123456'
    assert store.value['verified'] is True


def test_home_without_saved_recovery_submission_cannot_adopt_master():
    store = Store({'identity': 'account:device', 'pin': '123456', 'verified': False, 'words': WORDS})
    flow = controller(store, {'Home', 'Cubes', 'Settings'})
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'PHONE_MASTER_NOT_OWNED'


def test_missing_keychain_item_with_saved_locator_cannot_regenerate_pin(tmp_path):
    reply = {'value': {'pin': '123456'}, 'sha256': 'a'*64, 'service': 'qa-service', 'account': 'qa-account'}
    store = SecretStore(SimpleNamespace(call=lambda *a, **k: reply), tmp_path / 'locator.json')
    assert store.load() == {'pin': '123456'}
    assert '123456' not in (tmp_path / 'locator.json').read_text()
    reply.update(value=None, sha256=None)
    with pytest.raises(Failed) as error:
        store.load()
    assert error.value.code == 'SECRET_MISSING'
