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
    recovery_xml = xml([f'{i + 1} {v}' for (i, v) in enumerate(WORDS[:-1])])
    with pytest.raises(Blocked) as error:
        recovery_words(recovery_xml)
    assert error.value.code == 'RECOVERY_WORDS'


def test_conflicting_recovery_indices_fail():
    recovery_xml = xml(['1 abandon', '1 ability'])
    with pytest.raises(Failed) as error:
        recovery_words(recovery_xml)
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


@pytest.mark.parametrize('auto_home', [False, True])
def test_initial_setup_persists_pin_and_all_recovery_words_before_using_them(monkeypatch, auto_home):
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
            view[:] = ['Home', 'Cubes', 'Settings'] if auto_home else ['Go to home']
        elif label == 'Go to home':
            assert store.value['verified'] is True
            view[0] = 'Home'
        else:
            pytest.fail('Unexpected click')
    phone = SimpleNamespace(device_digest='device', labels=lambda: set(view), click=click, click_card=click, driver=source)
    flow = Onboarding(phone, store, 'account', 1)
    flow.fields = lambda: fields
    flow.reveal = lambda title: SimpleNamespace(click=lambda: click(title))
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


CHOOSER = {'Set up your keychain', 'Use a PIN', 'Use a passkey', 'I have a recovery phrase'}


def uncommitted_state():
    return {'identity': 'account:device', 'pin': '123456', 'verified': False,
            'entered_Create a PIN': True, 'entered_Confirm your PIN': True}


def test_fresh_chooser_archives_uncommitted_generation_before_any_ui_action():
    store = Store(uncommitted_state())
    flow = controller(store, CHOOSER)
    flow.phone.build = 37
    def click_card(title):
        assert title == 'Use a PIN'
        assert store.value['pin'] == '123456'
        assert 'entered_Create a PIN' not in store.value
        old = store.value['abandoned_setup_generations'][0]
        assert old['entered_Create a PIN'] is True
        assert old['entered_Confirm your PIN'] is True
        assert old['observed_build'] == 37
        raise Blocked('TEST_INTERRUPTION', 'Simulated interruption after durable archive.')
    flow.phone.click_card = click_card
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'TEST_INTERRUPTION'
    # Restarting before the first new entry does not append another generation.
    resumed = controller(store, CHOOSER)
    resumed.resume_uncommitted_setup(store.load(), CHOOSER)
    assert len(store.writes) == 1


@pytest.mark.parametrize('change', [
    {'entered_Confirm your PIN': False}, {'words': WORDS},
    {'recovery_submission_started': True}, {'pending_attempt': 'sign:run'},
    {'verified': True}, {'abandoned_setup_generations': [{'retained': True}]},
])
def test_chooser_cannot_restart_partial_committed_or_repeated_setup(change):
    state = uncommitted_state() | change
    store = Store(state)
    flow = controller(store, CHOOSER)
    saved_state = store.load()
    with pytest.raises(Blocked) as error:
        flow.resume_uncommitted_setup(saved_state, CHOOSER)
    assert error.value.code == 'SETUP_PIN_UNCERTAIN'
    assert store.writes == []


def test_uncommitted_generation_requires_the_complete_fresh_chooser():
    store = Store(uncommitted_state())
    flow = controller(store, {'Use a PIN'})
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'SETUP_PIN_UNCERTAIN'
    assert store.writes == []


class InputField:
    def __init__(self, kind='XCUIElementTypeTextField', descendants=()):
        self.kind, self.descendants, self.actions = kind, list(descendants), []

    def get_attribute(self, name):
        assert name == 'type'
        return self.kind

    def find_elements(self, by, query):
        assert by == 'xpath'
        assert query == 'descendant::XCUIElementTypeTextField | descendant::XCUIElementTypeSecureTextField'
        return self.descendants

    def click(self):
        self.actions.append('click')

    def clear(self):
        self.actions.append('clear')

    def send_keys(self, value):
        self.actions.append(('input', value))


def field_controller(candidates):
    flow = controller(Store())
    flow.phone.driver = SimpleNamespace(find_elements=lambda *args: candidates)
    return flow


@pytest.mark.parametrize('kind', ['XCUIElementTypeSecureTextField', 'XCUIElementTypeTextField'])
def test_protection_wrapper_is_excluded_from_three_recovery_fields(kind):
    children = [InputField() for _ in range(3)]
    wrapper = InputField(kind, children)
    assert field_controller([wrapper, *children]).fields() == children
    assert wrapper.actions == []


def test_protection_wrapper_never_receives_pin_but_real_secure_leaf_does():
    leaf = InputField('XCUIElementTypeSecureTextField')
    wrapper = InputField('XCUIElementTypeSecureTextField', [leaf])
    field_controller([wrapper, leaf]).fill_one('123456')
    assert wrapper.actions == []
    assert leaf.actions == ['click', 'clear', ('input', '123456')]


def test_unprotected_wrapper_never_receives_signer_name():
    leaf = InputField()
    wrapper = InputField('XCUIElementTypeTextField', [leaf])
    field_controller([wrapper, leaf]).fill_one('QA synthetic signer')
    assert wrapper.actions == []
    assert leaf.actions == ['click', 'clear', ('input', 'QA synthetic signer')]


@pytest.mark.parametrize('operation,code,preceding', [
    ('find_elements', 'PHONE_INPUT_QUERY', []),
    ('click', 'PHONE_INPUT_FOCUS', []),
    ('clear', 'PHONE_INPUT_CLEAR', ['click']),
    ('send_keys', 'PHONE_INPUT_DISPATCH', ['click', 'clear']),
])
def test_input_failures_report_only_operation_without_replay(operation, code, preceding):
    leaf = InputField('XCUIElementTypeSecureTextField')
    flow = field_controller([leaf])
    calls = []

    def fail(*args):
        calls.append(operation)
        raise RuntimeError('synthetic-private-input-and-device-data')

    setattr(flow.phone.driver if operation == 'find_elements' else leaf, operation, fail)
    with pytest.raises(Blocked) as error:
        flow.fill_one('synthetic-private-input')
    assert error.value.code == code
    assert 'synthetic-private' not in str(error.value)
    assert leaf.actions == preceding
    assert calls == [operation]


def test_hidden_or_disabled_descendant_does_not_make_wrapper_editable():
    # Only the wrapper passed the top-level visible+enabled filter; its hidden
    # input still identifies it as a container and must exclude it.
    wrapper = InputField('XCUIElementTypeSecureTextField', [InputField()])
    flow = field_controller([wrapper])
    with pytest.raises(Blocked) as error:
        flow.fill_one('123456')
    assert error.value.code == 'PHONE_FIELD'
    assert wrapper.actions == []


def test_multiple_real_fields_never_fall_back_to_first_input():
    children = [InputField(), InputField('XCUIElementTypeSecureTextField')]
    wrapper = InputField('XCUIElementTypeSecureTextField', children)
    flow = field_controller([wrapper, *children])
    with pytest.raises(Blocked) as error:
        flow.fill_one('123456')
    assert error.value.code == 'PHONE_FIELD'
    assert all(not field.actions for field in [wrapper, *children])


def test_signer_creation_follows_normal_confirmation_sheet_before_name_and_pin():
    store = Store({'identity': 'account:device', 'pin': '123456', 'verified': True})
    view, clicks, filled, artifacts = [{'Home', 'Add signer key'}], [], [], {}
    def click(label):
        clicks.append(label)
        if label == 'Add signer key':
            view[0] = {'Create new signer key', 'Create Key', 'Cancel'}
        elif label == 'Create Key':
            view[0] = {'Name your signer key'}
        elif label == 'Create signer key':
            assert artifacts['phone_key_creation_started'] is True
            assert filled == ['QA run']
            view[0] = {'Enter your PIN'}
        elif label == 'Unlock':
            assert artifacts['phone_key_pin_entered'] is True
            assert store.value['pending_attempt'] == 'key:QA run'
            assert filled == ['QA run', '123456']
            view[0] = {'Signer key created', 'Back to home'}
        elif label == 'Back to home':
            view[0] = {'Home'}
    phone = SimpleNamespace(device_digest='device', labels=lambda: view[0],
                            restore_navigation=lambda: None, click=click)
    flow = Onboarding(phone, store, 'account', 1)
    flow.fill_one = filled.append
    journal = SimpleNamespace(data={'artifacts': artifacts}, artifact=lambda key, value: artifacts.update({key: value}))
    flow.create_key('QA run', '123456', journal)
    assert clicks == ['Home', 'Add signer key', 'Create Key', 'Create signer key', 'Unlock', 'Back to home']


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


def test_success_screen_without_saved_submission_cannot_verify_setup():
    store = Store(uncommitted_state() | {'words': WORDS})
    flow = controller(store, {'Go to home'})
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'RECOVERY_UNVERIFIED'
    assert store.writes == []


def test_reveal_targets_the_scroll_view_instead_of_the_keyboard(monkeypatch):
    monkeypatch.setattr('harness.onboarding.time.sleep', lambda _: None)
    views = iter([set(), set(), {'Confirm and finish setup'}])
    gestures = []
    flow = controller(Store())
    flow.phone.labels = lambda: next(views)
    controls = iter([[], [], [SimpleNamespace(id='confirmation-button')]])
    flow.phone.matching_controls = lambda _: next(controls)
    flow.phone.driver = SimpleNamespace(
        find_elements=lambda *args: [SimpleNamespace(id='recovery-scroll')],
        execute_script=lambda command, arguments: gestures.append((command, arguments)))
    flow.reveal('Confirm and finish setup')
    assert gestures == [('mobile: swipe', {'direction': 'up', 'elementId': 'recovery-scroll'})] * 2


@pytest.mark.parametrize('count', [0, 2])
def test_reveal_never_guesses_an_absent_or_ambiguous_scroll_view(count):
    flow = controller(Store())
    flow.phone.matching_controls = lambda _: []
    flow.phone.driver = SimpleNamespace(
        find_elements=lambda *args: [object()] * count,
        execute_script=lambda *args: pytest.fail('must not send unscoped gesture'))
    with pytest.raises(Blocked) as error:
        flow.reveal('Confirm and finish setup')
    assert error.value.code == 'PHONE_SCROLL'


def test_reveal_ignores_ancestor_text_until_the_actual_control_exists(monkeypatch):
    monkeypatch.setattr('harness.onboarding.time.sleep', lambda _: None)
    flow = controller(Store(), {'Confirm and finish setup'})
    button, gestures = object(), []
    controls = iter([[], [button]])
    flow.phone.matching_controls = lambda _: next(controls)
    flow.phone.driver = SimpleNamespace(
        find_elements=lambda *args: [SimpleNamespace(id='recovery-scroll')],
        execute_script=lambda command, args: gestures.append(args))
    assert flow.reveal('Confirm and finish setup') is button
    assert gestures == [{'direction': 'up', 'elementId': 'recovery-scroll'}]


def test_reveal_rejects_ambiguous_controls_without_scrolling_or_tapping():
    flow = controller(Store(), {'Confirm and finish setup'})
    flow.phone.matching_controls = lambda _: [object(), object()]
    with pytest.raises(Blocked) as error:
        flow.reveal('Confirm and finish setup')
    assert error.value.code == 'PHONE_SELECTOR'


@pytest.mark.parametrize('dispatch_status', [None, 'dispatch_pending'])
def test_uncertain_recovery_submission_never_refills_or_replays(dispatch_status):
    state = uncommitted_state() | {'words': WORDS, 'recovery_submission_started': True}
    if dispatch_status:
        state['recovery_dispatch_status'] = dispatch_status
    store = Store(state)
    flow = controller(store, {'Check what you wrote down', 'Confirm and finish setup'})
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'RECOVERY_SUBMISSION_UNCERTAIN'
    assert store.writes == []


def test_lost_recovery_click_response_preserves_intent_and_never_replays():
    store = Store(uncommitted_state() | {'words': WORDS})
    flow = controller(store, {'Check what you wrote down'})
    flow.phone.driver = SimpleNamespace(page_source=xml(['Word 1', 'Word 2', 'Word 3']))
    flow.fields = lambda: [InputField() for _ in range(3)]
    clicks = []
    def click():
        assert store.value['recovery_submission_started'] is True
        assert store.value['recovery_dispatch_status'] == 'dispatch_pending'
        clicks.append(True)
        raise RuntimeError('Synthetic lost response after dispatch')
    flow.reveal = lambda _: SimpleNamespace(click=click)
    with pytest.raises(RuntimeError):
        flow.setup()
    flow = controller(store, {'Check what you wrote down'})
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'RECOVERY_SUBMISSION_UNCERTAIN'
    assert clicks == [True]


def test_reveal_preserves_foreground_authentication_guard():
    flow = controller(Store())
    def labels():
        raise Blocked('PHONE_AUTH', 'Synthetic OS prompt')
    flow.phone.labels = labels
    flow.phone.matching_controls = lambda _: pytest.fail('must check foreground first')
    with pytest.raises(Blocked) as error:
        flow.reveal('Confirm and finish setup')
    assert error.value.code == 'PHONE_AUTH'


@pytest.mark.parametrize('footer,heading', [
    ('Confirm and finish setup', 'Check what you wrote down'),
    ('I have written them down', 'Write down these 12 words'),
])
def test_saved_recovery_page_resumes_from_its_footer_without_resetting_credentials(footer, heading):
    store = Store(uncommitted_state() | {'words': WORDS})
    flow = controller(store, {footer})
    def reveal(label, direction):
        assert (label, direction) == (heading, 'down')
        raise Blocked('TEST_STOP', 'Stop after identifying the safe resume gesture.')
    flow.reveal = reveal
    with pytest.raises(Blocked) as error:
        flow.setup()
    assert error.value.code == 'TEST_STOP'
    assert store.writes == []


def test_missing_keychain_item_with_saved_locator_cannot_regenerate_pin(tmp_path):
    reply = {'value': {'pin': '123456'}, 'sha256': 'a'*64, 'service': 'qa-service', 'account': 'qa-account'}
    store = SecretStore(SimpleNamespace(call=lambda *a, **k: reply), tmp_path / 'locator.json')
    assert store.load() == {'pin': '123456'}
    assert '123456' not in (tmp_path / 'locator.json').read_text()
    reply.update(value=None, sha256=None)
    with pytest.raises(Failed) as error:
        store.load()
    assert error.value.code == 'SECRET_MISSING'


def test_attach_uses_phone_flow_and_persists_intent_before_cube_selection():
    flow = controller(Store())
    journal = SimpleNamespace(data={'artifacts': {}})
    journal.artifact = lambda k, v: journal.data['artifacts'].update({k: v})
    actions = []
    flow.phone.restore_navigation = lambda: actions.append('home-navigation')
    flow.phone.click = lambda label: actions.append(label)
    flow.phone.home_key_card = lambda name, **kw: SimpleNamespace(click=lambda: actions.append((name, kw)))
    flow.wait = lambda title: actions.append(('wait', title))
    flow.reveal = lambda title: SimpleNamespace(click=lambda: actions.append(title))
    def select():
        assert journal.data['artifacts']['phone_key_attachment_started'] is True
        actions.append('cube-selected')
    def find(title, **kw):
        assert not journal.data['artifacts'].get('phone_key_attachment_started')
        assert title == 'QA synthetic cube'
        assert kw == {}
        return SimpleNamespace(click=select)
    flow.phone.card_control = find
    flow.attach_key('QA synthetic key', 'QA synthetic cube', journal)
    assert actions == ['home-navigation', 'Home', ('QA synthetic key', {'timeout': flow.deadline}),
                       ('wait', 'Signer key'), 'Attach to Cube',
                       ('wait', 'Pick a cube on the testnet4 network.'), 'cube-selected']


class RevisionStore(Store):
    def __init__(self, value):
        super().__init__(value)
        self.sha256 = 'original-revision'

    def save(self, value):
        super().save(value)
        self.sha256 = 'revision-' + str(len(self.writes))


def key_recovery():
    store = RevisionStore({'identity': 'account:device', 'verified': True,
        'pin': '123456', 'words': WORDS, 'pending_attempt': 'key:old'})
    flow = controller(store)
    receipt = {'recovery_id': 'generation-1', 'phase': 'ready_for_reconciliation',
        'creation_started': False, 'pin_entered': False,
        'binding': {'identity': 'account:device', 'original_purpose': 'key:old',
            'new_purpose': 'key:new', 'original_credential_revision': 'original-revision',
            'original_key_label': 'old', 'new_key_label': 'new', 'authorization': 'explicit-user-reference'}}
    return flow, store, receipt


def test_quarantine_cas_preserves_history_and_never_makes_credential_globally_idle():
    flow, store, receipt = key_recovery()
    assert flow.reserve_key_generation(receipt) == 'reserved'
    assert len(store.writes) == 1
    state = store.value
    assert 'pending_attempt' not in state
    assert state['pin'] == '123456'
    assert state['words'] == WORDS
    assert state['quarantined_attempts'][0]['purpose'] == 'key:old'
    assert state['quarantined_attempts'][0]['disposition'] == 'outcome_unresolved'
    assert state['key_generation_reservation']['status'] == 'reserved'
    with pytest.raises(Blocked) as error:
        flow.pin_attempt('key:unrelated')
    assert error.value.code == 'KEY_GENERATION_RESERVED'
    assert flow.reserve_key_generation(receipt) == 'reserved'
    assert len(store.writes) == 1


@pytest.mark.parametrize('mutation', ['revision', 'purpose', 'identity', 'phase', 'authorization', 'same-purpose', 'history'])
def test_quarantine_requires_original_revision_identity_authorization_and_boundary(mutation):
    flow, store, receipt = key_recovery()
    if mutation == 'revision': store.sha256 = 'different'
    if mutation == 'purpose': store.value['pending_attempt'] = 'key:foreign'
    if mutation == 'identity': store.value['identity'] = 'other:device'
    if mutation == 'phase': receipt['phase'] = 'planned'
    if mutation == 'authorization': receipt['binding']['authorization'] = ''
    if mutation == 'same-purpose': receipt['binding']['new_purpose'] = 'key:old'
    if mutation == 'history': store.value['quarantined_attempts'] = [{'old': True}]
    before = deepcopy(store.value)
    with pytest.raises((Blocked, Failed)):
        flow.reserve_key_generation(receipt)
    assert store.value == before
    assert store.writes == []


def test_reservation_consumption_is_once_only_and_idempotent_recovery_never_clears_new_pending():
    flow, store, receipt = key_recovery()
    flow.reserve_key_generation(receipt)
    receipt.update(phase='creation_dispatched', creation_started=True, pin_entered=True)
    assert flow.reserved_key_pin(receipt) == '123456'
    assert store.value['pending_attempt'] == 'key:new'
    assert store.value['key_generation_reservation']['status'] == 'consumed'
    assert flow.reserve_key_generation(receipt) == 'consumed'
    assert len(store.writes) == 2
    with pytest.raises(Blocked):
        flow.reserved_key_pin(receipt)
    flow.reconcile_pin('key:old')
    assert store.value['pending_attempt'] == 'key:new'
    flow.reconcile_pin('key:new')
    assert 'pending_attempt' not in store.value
    assert store.value['key_generation_reservation']['status'] == 'completed'
    assert store.value['quarantined_attempts'][0]['disposition'] == 'outcome_unresolved'
    assert flow.reserve_key_generation(receipt) == 'completed'
    assert flow.pin_attempt('sign:verified-new-run') == '123456'
    assert store.value['quarantined_attempts'][0]['purpose'] == 'key:old'


@pytest.mark.parametrize('field', ['purpose', 'authorization', 'original_credential_revision', 'disposition', 'binding_sha256'])
def test_changed_quarantine_history_cannot_be_reused(field):
    flow, store, receipt = key_recovery()
    flow.reserve_key_generation(receipt)
    store.value['quarantined_attempts'][0][field] = 'changed'
    with pytest.raises(Failed):
        flow.reserve_key_generation(receipt)
    assert len(store.writes) == 1


def recovery_creation_phone(flow, receipt, *, pin_value='6-digit PIN', name='new', lose_input=False):
    from harness.phone import Phone, BUNDLE
    process = {'pid': 7, 'bundle_id': BUNDLE}
    receipt.update(phase='reserved', current_process=process)
    calls = []
    phone = SimpleNamespace(build=38, process_identity=lambda: process, restore_navigation=lambda: calls.append('restore'),
        click=calls.append, labels=lambda: {'Add signer key', 'Enter your PIN'}, reject_pin_error=Phone.reject_pin_error)
    flow.phone = phone
    flow.wait = lambda *choices: {choices[0]}
    flow.fill_one = lambda value: calls.append(('name', value))
    def dispatch(value):
        assert flow.store.value['pending_attempt'] == 'key:new'
        assert flow.store.value['key_generation_reservation']['status'] == 'consumed'
        calls.append('pin-dispatch')
        if lose_input:
            raise RuntimeError('Synthetic lost input response')
    name_field = SimpleNamespace(get_attribute=lambda _: name)
    pin_field = SimpleNamespace(get_attribute=lambda _: pin_value, click=lambda: calls.append('focus'), send_keys=dispatch)
    fields = iter([[name_field], [pin_field]])
    flow.fields = lambda: next(fields)
    return calls


@pytest.mark.parametrize('case', ['name', 'nonempty', 'lost-input', 'success'])
def test_new_generation_requires_exact_name_empty_prompt_and_durable_single_input(tmp_path, case):
    from harness.state import Journal
    flow, store, receipt = key_recovery()
    flow.reserve_key_generation(receipt)
    calls = recovery_creation_phone(flow, receipt, name='wrong' if case == 'name' else 'new',
        pin_value='•••' if case == 'nonempty' else '6-digit PIN', lose_input=case == 'lost-input')
    journal = Journal(tmp_path / case, {})
    try:
        action = lambda: flow.create_recovery_key('new', journal, receipt,
            before_create=lambda: calls.append('guard-create'), before_pin=lambda: calls.append('guard-pin'))
        if case == 'success':
            action()
        else:
            with pytest.raises(Blocked):
                action()
        if case in {'name', 'nonempty'}:
            assert 'pin-dispatch' not in calls
            assert store.value['key_generation_reservation']['status'] == 'reserved'
        else:
            assert calls.count('pin-dispatch') == 1
            assert receipt['pin_entered'] is True
            assert store.value['pending_attempt'] == 'key:new'
            with pytest.raises(Blocked):
                action()
            assert calls.count('pin-dispatch') == 1
        assert '123456' not in journal.path.read_text()
    finally:
        journal.close()



def test_quarantine_history_must_still_match_at_first_input():
    flow, store, receipt = key_recovery()
    flow.reserve_key_generation(receipt)
    receipt.update(phase='creation_dispatched', creation_started=True, pin_entered=True)
    store.value['quarantined_attempts'] = []
    with pytest.raises(Blocked):
        flow.reserved_key_pin(receipt)
    assert store.value['key_generation_reservation']['status'] == 'reserved'
    assert 'pending_attempt' not in store.value


@pytest.mark.parametrize('value', [None, '', '6-digit PIN'])
def test_same_process_generation_resumes_first_input_without_navigation(tmp_path, value):
    from harness.state import Journal
    flow, store, receipt = key_recovery()
    flow.reserve_key_generation(receipt)
    calls = recovery_creation_phone(flow, receipt, pin_value=value)
    flow.fields()  # Discard the synthetic name field; creation already happened.
    receipt.update(phase='creation_dispatched', creation_started=True)
    journal = Journal(tmp_path / 'resume', {})
    try:
        flow.resume_recovery_key_pin(journal, receipt, before_pin=lambda: calls.append('guard-pin'))
        assert calls == ['guard-pin', 'focus', 'pin-dispatch', 'Unlock', 'Back to home']
        assert receipt['pin_entered'] is True
        assert store.value['key_generation_reservation']['status'] == 'consumed'
        with pytest.raises(Blocked):
            flow.resume_recovery_key_pin(journal, receipt, before_pin=lambda: None)
        assert calls.count('pin-dispatch') == 1
    finally:
        journal.close()


@pytest.mark.parametrize('mutation', ['process', 'build', 'pin', 'creation', 'phase', 'consumed', 'missing', 'pending', 'identity', 'nonempty', 'lost-input'])
def test_generation_continuation_never_replays_or_recreates(tmp_path, mutation):
    from harness.state import Journal
    flow, store, receipt = key_recovery()
    flow.reserve_key_generation(receipt)
    calls = recovery_creation_phone(flow, receipt, pin_value='••' if mutation == 'nonempty' else None,
        lose_input=mutation == 'lost-input')
    flow.fields()
    receipt.update(phase='creation_dispatched', creation_started=True)
    if mutation == 'process': receipt['current_process'] = {'pid': 8, 'bundle_id': 'io.coincube.keychainApp'}
    if mutation == 'build': flow.phone.build = 39
    if mutation == 'pin': receipt['pin_entered'] = True
    if mutation == 'creation': receipt['creation_started'] = False
    if mutation == 'phase': receipt['phase'] = 'reserved'
    if mutation == 'consumed':
        store.value['key_generation_reservation']['status'] = 'consumed'
        store.value['pending_attempt'] = 'key:new'
    if mutation == 'missing': store.value.pop('key_generation_reservation')
    if mutation == 'pending': store.value['pending_attempt'] = 'key:foreign'
    if mutation == 'identity': store.value['identity'] = 'other:device'
    journal = Journal(tmp_path / mutation, {})
    try:
        with pytest.raises((Blocked, Failed)):
            flow.resume_recovery_key_pin(journal, receipt, before_pin=lambda: None)
        assert 'restore' not in calls
        assert 'Create signer key' not in calls
        assert calls.count('pin-dispatch') == (1 if mutation == 'lost-input' else 0)
        if mutation == 'lost-input':
            with pytest.raises(Blocked):
                flow.resume_recovery_key_pin(journal, receipt, before_pin=lambda: None)
            assert calls.count('pin-dispatch') == 1
            assert store.value['key_generation_reservation']['status'] == 'consumed'
    finally:
        journal.close()


def test_attachment_dispatch_marker_blocks_before_navigation_or_lookup():
    flow = controller(Store())
    journal = SimpleNamespace(data={'artifacts': {'phone_key_attachment_started': True}})
    flow.phone.restore_navigation = lambda: pytest.fail('An attachment retry must not navigate')
    with pytest.raises(Blocked) as error:
        flow.attach_key('QA exact replacement', 'QA exact cube', journal)
    assert error.value.code == 'KEY_ATTACHMENT_PENDING'


def test_uncertain_attachment_selection_is_never_repeated():
    flow = controller(Store())
    journal = SimpleNamespace(data={'artifacts': {}})
    journal.artifact = lambda k, v: journal.data['artifacts'].update({k: v})
    calls = []
    flow.phone.restore_navigation = lambda: calls.append('navigation')
    flow.phone.click = lambda label: calls.append(label)
    flow.phone.home_key_card = lambda name, **kw: SimpleNamespace(click=lambda: calls.append(name))
    flow.wait = lambda *args: None
    flow.reveal = lambda title: SimpleNamespace(click=lambda: calls.append(title))
    def selection():
        assert journal.data['artifacts']['phone_key_attachment_started'] is True
        calls.append('selection')
        raise RuntimeError('Synthetic lost selection response')
    flow.phone.card_control = lambda title: SimpleNamespace(click=selection)
    with pytest.raises(RuntimeError):
        flow.attach_key('QA exact replacement', 'QA exact cube', journal)
    original = list(calls)
    with pytest.raises(Blocked) as error:
        flow.attach_key('QA exact replacement', 'QA exact cube', journal)
    assert error.value.code == 'KEY_ATTACHMENT_PENDING'
    assert calls == original
    assert calls.count('selection') == 1
