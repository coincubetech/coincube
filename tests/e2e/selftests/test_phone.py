from types import SimpleNamespace

import pytest

from harness.phone import Phone
from harness.state import Blocked, Failed, Journal


INTENT = {"outputs": [{"address": "tb1fixture-recipient", "sats": 10000},
                      {"address": "tb1fixture-change", "sats": 989388}]}
REVIEW = {"Approve transaction", "testnet4", "612 sats", "10000 sats", "989388 sats",
          "tb1fixture-recipient", "tb1fixture-change"}
HOME = {"Home", "Cubes", "Settings"}
INCOMING = {"Incoming transaction", "Review the details before approving", "View transaction", "Not now"}


def scripted_phone(views):
    phone = Phone(SimpleNamespace(execute_script=lambda *args: None), 35)
    screens = iter(views)
    phone.labels = lambda: next(screens)
    clicks = []
    phone.click = clicks.append
    phone.click_line = clicks.append
    phone.refresh = lambda: None
    return phone, clicks


@pytest.mark.parametrize("restored,expected", [
    ([{"Signature Submitted", "Done"}], ["Done"]),
    ([{"Transaction approved", "Done"}], ["Done"]),
    ([REVIEW | {"Back"}], ["Back"]),
    ([{"Enter PIN", "Back", "1", "2"}, REVIEW | {"Back"}], ["Back", "Back"]),
    ([{"Create new signer key", "Create Key", "Cancel", "Home", "Cubes", "Settings"}], ["Cancel"]),
])
def test_fixture_check_resumes_without_approval_or_pin(restored, expected, monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone(restored + [
        {"Home", "Cubes", "Settings"}, {"Testnet4"}, {"Disposable QA Cube"},
    ])
    assert phone.check_fixture("Disposable QA Cube")["cube_visible"] is True
    assert clicks == expected + ["Settings", "Cubes", "Home"]


def test_unknown_restored_screen_never_guesses_navigation(monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone([{"Approve", "Try Again", "Done", "Enter PIN"}] * 16)
    with pytest.raises(Blocked) as error:
        phone.restore_navigation()
    assert error.value.code == "PHONE_NAVIGATION"
    assert clicks == []


def test_result_then_unlabeled_pending_page_restarts_without_reset(monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone([
        {"Signature Submitted", "Done"}, {"Pending Requests"}, set(),
        {"Home", "Cubes", "Settings"}, {"Testnet4"}, {"Disposable QA Cube"},
    ])
    lifecycle = []
    phone.driver.terminate_app = lambda bundle: lifecycle.append(("terminate", bundle))
    phone.driver.activate_app = lambda bundle: lifecycle.append(("activate", bundle))
    phone.check_fixture("Disposable QA Cube")
    assert clicks == ["Done", "Settings", "Cubes", "Home"]
    assert lifecycle == [("terminate", "io.coincube.keychainApp"), ("activate", "io.coincube.keychainApp")]


def test_pending_page_restart_is_bounded(monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone([{"Pending Requests"}] * 16)
    lifecycle = []
    phone.driver.terminate_app = lambda _: lifecycle.append("terminate")
    phone.driver.activate_app = lambda _: lifecycle.append("activate")
    with pytest.raises(Blocked) as error:
        phone.restore_navigation()
    assert error.value.code == "PHONE_NAVIGATION"
    assert lifecycle == ["terminate", "activate"]
    assert clicks == []


def test_approval_never_selects_an_unrelated_request(tmp_path):
    phone, clicks = scripted_phone([HOME, {"some other request"}])
    journal = Journal(tmp_path / "run", {})
    try:
        with pytest.raises(Blocked) as error:
            phone.approve("fixture", INTENT, 612, None, journal, timeout=0)
        assert error.value.code == "REQUEST_DELIVERY"
        assert "Approve" not in clicks
        assert not journal.data["artifacts"].get("approval_started")
    finally:
        journal.close()


def test_uncertain_approval_never_retries_pin(tmp_path):
    phone, clicks = scripted_phone([{"Testnet4 QA fixture"}, REVIEW])
    journal = Journal(tmp_path / "run", {})
    journal.artifact("approval_started", True)
    try:
        with pytest.raises(Blocked) as error:
            phone.approve("fixture", INTENT, 612, None, journal, timeout=2)
        assert error.value.code == "APPROVAL_UNCERTAIN"
        assert "Approve" not in clicks
    finally:
        journal.close()


def test_wrong_phone_summary_never_approves(tmp_path, monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    wrong = REVIEW - {"tb1fixture-recipient"}
    phone, clicks = scripted_phone([HOME, {"Testnet4 QA fixture"}] + [wrong] * 8)
    journal = Journal(tmp_path / "run", {})
    try:
        with pytest.raises(Failed) as error:
            phone.approve("fixture", INTENT, 612, None, journal, timeout=2)
        assert error.value.code == "PHONE_REVIEW"
        assert "Approve" not in clicks
    finally:
        journal.close()


def test_real_banner_and_pin_flow_use_exact_labels(tmp_path, monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone([HOME, {"1 signing request waiting"}, {"Testnet4 QA fixture"}, REVIEW,
                                   {"Enter PIN"}, {"Signature Submitted"}])
    pin = tmp_path / "pin"
    pin.write_text("987654")
    pin.chmod(0o600)
    journal = Journal(tmp_path / "run", {})
    try:
        phone.approve("fixture", INTENT, 612, str(pin), journal, timeout=5)
        assert clicks == ["Home", "1 signing request waiting", "Testnet4 QA fixture", "Approve", *"987654"]
        assert journal.data["artifacts"]["approval_started"] is True
        assert "987654" not in journal.path.read_text()
    finally:
        journal.close()


def test_key_creation_success_returns_home_without_retrying_creation(monkeypatch):
    monkeypatch.setattr('harness.phone.time.sleep', lambda _: None)
    phone, clicks = scripted_phone([{'Signer key created', 'Back to home'}, {'Home', 'Cubes', 'Settings'}])
    phone.restore_navigation()
    assert clicks == ['Back to home']


@pytest.mark.parametrize('survives', [True, False])
def test_pin_resume_probe_records_only_observed_lifecycle_survival(tmp_path, monkeypatch, survives):
    ticks = iter([0, 0, 4])
    monkeypatch.setattr('harness.phone.time.monotonic', lambda: next(ticks))
    monkeypatch.setattr('harness.phone.time.sleep', lambda _: None)
    phone, clicks = scripted_phone([{'Enter PIN'} if survives else {'Approve transaction'}] * 2)
    lifecycle = []
    phone.driver.execute_script = lambda *args: lifecycle.append(args)
    phone.driver.activate_app = lambda bundle: lifecycle.append(('activate', bundle))
    journal = Journal(tmp_path / 'probe', {})
    try:
        if survives:
            phone.verify_pin_resume(journal)
            result = journal.data['artifacts']['pin_resume_probe']
            assert result['status'] == 'passed'
            assert result['home_refresh_proven'] is False
        else:
            with pytest.raises(Failed) as error:
                phone.verify_pin_resume(journal)
            assert error.value.code == 'PIN_CHALLENGE_DISMISSED'
        assert clicks == []
        assert lifecycle[0] == ('mobile: backgroundApp', {'seconds': 1})
        assert lifecycle[1] == ('activate', 'io.coincube.keychainApp')
    finally:
        journal.close()


def test_merged_setup_card_matches_only_complete_title_line():
    from appium.webdriver.common.appiumby import AppiumBy
    clicked, predicates = [], []
    def find(by, predicate):
        assert by == AppiumBy.IOS_PREDICATE
        predicates.append(predicate)
        return [SimpleNamespace(click=lambda: clicked.append(True))]
    phone = Phone(SimpleNamespace(find_elements=find), 36)
    phone.click_card('Use a PIN')
    assert clicked == [True]
    assert 'BEGINSWITH "Use a PIN\\n"' in predicates[0]
    assert 'CONTAINS' not in predicates[0]
    assert 'visible == true AND enabled == true' in predicates[0]
    assert 'XCUIElementTypeStaticText' in predicates[0]
    with pytest.raises(Failed):
        phone.click_card('1')
    assert len(predicates) == 1


def test_merged_card_selector_refuses_ambiguous_ancestors():
    phone = Phone(SimpleNamespace(find_elements=lambda *args: [object(), object()]), 36)
    with pytest.raises(Blocked) as error:
        phone.click_card('Use a PIN')
    assert error.value.code == 'PHONE_SELECTOR'


def test_setup_card_waits_for_readiness_without_broadening_predicate(monkeypatch):
    waits, clicks, queries = [], [], []
    results = iter([[], [], [SimpleNamespace(click=lambda: clicks.append(True))]])
    def find(by, predicate):
        queries.append((by, predicate))
        return next(results)
    monkeypatch.setattr('harness.phone.time.sleep', waits.append)
    phone = Phone(SimpleNamespace(find_elements=find), 37)
    phone.click_card('Use a PIN')
    assert waits == [.25, .25]
    assert clicks == [True]
    assert len(set(queries)) == 1


def test_setup_card_absence_is_bounded_and_never_clicks():
    phone = Phone(SimpleNamespace(find_elements=lambda *args: []), 37)
    with pytest.raises(Blocked) as error:
        phone.click_card('Use a PIN', timeout=0)
    assert error.value.code == 'PHONE_SELECTOR'


def test_setup_card_never_retries_an_uncertain_click(monkeypatch):
    clicks = []
    def click():
        clicks.append(True)
        raise RuntimeError('Synthetic lost click response')
    monkeypatch.setattr('harness.phone.time.sleep', lambda _: pytest.fail('must not retry'))
    phone = Phone(SimpleNamespace(find_elements=lambda *args: [SimpleNamespace(click=click)]), 37)
    with pytest.raises(RuntimeError):
        phone.click_card('Use a PIN')
    assert clicks == [True]


def test_gesture_card_uses_only_explicit_container_and_exact_title_line():
    queries, clicks = [], []
    def find(by, predicate):
        queries.append(predicate)
        return [SimpleNamespace(click=lambda: clicks.append(True))]
    phone = Phone(SimpleNamespace(find_elements=find), 37)
    control = phone.card_control('QA synthetic cube', container=True)
    assert clicks == []  # Caller can record durable intent before dispatch.
    control.click()
    assert clicks == [True]
    assert 'type == "XCUIElementTypeOther"' in queries[0]
    assert 'BEGINSWITH "QA synthetic cube\\n"' in queries[0]
    assert 'CONTAINS' not in queries[0]


def test_persisted_key_detail_with_disabled_back_relaunches_without_reset(monkeypatch):
    monkeypatch.setattr('harness.phone.time.sleep', lambda _: None)
    phone, clicks = scripted_phone([{'Signer key', 'Back'}, {'Home', 'Cubes', 'Settings'}])
    phone.matching_controls = lambda label: []
    lifecycle = []
    phone.driver.terminate_app = lambda bundle: lifecycle.append(('terminate', bundle))
    phone.driver.activate_app = lambda bundle: lifecycle.append(('activate', bundle))
    phone.restore_navigation()
    assert clicks == []
    assert lifecycle == [('terminate', 'io.coincube.keychainApp'), ('activate', 'io.coincube.keychainApp')]


def test_incoming_sheet_is_dismissed_before_exact_queue_selection(tmp_path, monkeypatch):
    monkeypatch.setattr('harness.phone.time.sleep', lambda _: None)
    phone, clicks = scripted_phone([INCOMING, HOME, INCOMING,
        {'1 signing request waiting'}, {'Testnet4 QA fixture'}, REVIEW, {'Signature Submitted'}])
    journal = Journal(tmp_path / 'incoming', {})
    try:
        phone.approve('fixture', INTENT, 612, None, journal, timeout=5)
        assert clicks == ['Not now', 'Home', 'Not now', '1 signing request waiting',
                          'Testnet4 QA fixture', 'Approve']
        assert 'View transaction' not in clicks
    finally:
        journal.close()


def test_not_now_on_unrelated_screen_is_not_dismissed():
    phone, clicks = scripted_phone([])
    assert not phone.dismiss_incoming({'Signer store needs repair', 'Not now'})
    assert clicks == []


def line_element(identifier, descendants=(), clicks=None):
    def find(by, query):
        assert by == 'xpath' and query == 'descendant::*'
        return list(descendants)
    return SimpleNamespace(id=identifier, find_elements=find,
                           click=lambda: clicks.append(identifier))


def test_request_line_selects_only_leaf_and_preserves_complete_line_boundaries():
    clicks, queries = [], []
    leaf = line_element('request-card', clicks=clicks)
    ancestor = line_element('screen', [leaf], clicks)
    def find(by, predicate):
        queries.append(predicate)
        return [ancestor, leaf]
    phone = Phone(SimpleNamespace(find_elements=find), 37)
    phone.click_line('Testnet4 QA fixture')
    assert clicks == ['request-card']
    query = queries[0]
    assert 'ENDS WITH' not in query
    assert 'ENDSWITH "\\nTestnet4 QA fixture"' in query
    assert 'CONTAINS "\\nTestnet4 QA fixture\\n"' in query
    assert 'CONTAINS "Testnet4 QA fixture"' not in query
    assert 'visible == true AND enabled == true' in query


@pytest.mark.parametrize('count', [0, 2])
def test_missing_or_duplicate_request_lines_never_tap(count):
    clicks = []
    elements = [line_element(str(i), clicks=clicks) for i in range(count)]
    phone = Phone(SimpleNamespace(find_elements=lambda *a: elements), 37)
    with pytest.raises(Blocked) as error:
        phone.click_line('Testnet4 QA fixture')
    assert error.value.code == 'PHONE_SELECTOR'
    assert clicks == []


def test_line_selector_cannot_be_used_for_pin_digits():
    phone = Phone(SimpleNamespace(), 37)
    with pytest.raises(Failed) as error:
        phone.click_line('1')
    assert error.value.code == 'PHONE_LINE'


def bound_phone():
    calls = []
    driver = SimpleNamespace(execute_script=lambda *args: {'bundleId': 'io.coincube.keychainApp', 'pid': 123},
        terminate_app=lambda bundle: calls.append('terminate'), activate_app=lambda bundle: calls.append('activate'))
    phone = Phone(driver, 38)
    phone.device_digest = 'synthetic-device'
    phone.matching_controls = lambda label: [object()]
    phone.labels = lambda: {'Enter PIN', 'signing-pin-session:session', 'signing-pin-entry:empty'}
    phone.click = calls.append
    return phone, calls


def bound_journal(tmp_path, phone):
    journal = Journal(tmp_path / 'bound', {})
    journal.artifact('approval_binding', {'session_id': 'session', 'request_id': 'request'})
    journal.artifact('approval_started', True)
    journal.artifact('approval_context', phone.approval_context('request', 'session', INTENT, 612, journal))
    return journal


@pytest.mark.parametrize('labels', [
    {'Enter PIN'},
    {'Enter PIN', 'signing-pin-session:other', 'signing-pin-entry:empty'},
    {'Enter PIN', 'signing-pin-session:session', 'signing-pin-entry:nonempty'},
    {'Enter PIN', 'signing-pin-session:session', 'signing-pin-entry:empty', 'Incorrect PIN'},
])
def test_bound_pin_never_enters_generic_wrong_nonempty_or_error_challenge(tmp_path, labels):
    phone, calls = bound_phone()
    journal = bound_journal(tmp_path, phone)
    phone.labels = lambda: labels
    try:
        with pytest.raises((Blocked, Failed)):
            phone.resume_first_pin('request', 'session', INTENT, 612, journal, 0,
                pin_provider=lambda: pytest.fail('credential must not be requested'), before_pin=lambda: True)
        assert calls == []
        assert 'pin_dispatch_started' not in journal.data['artifacts']
    finally:
        journal.close()


@pytest.mark.parametrize('changed', ['pid', 'device', 'build', 'intent', 'session', 'fee'])
def test_bound_pin_refuses_changed_approval_context(tmp_path, changed):
    phone, calls = bound_phone()
    journal = bound_journal(tmp_path, phone)
    intent, session, fee = INTENT, 'session', 612
    if changed == 'pid':
        phone.driver.execute_script = lambda *a: {'bundleId': 'io.coincube.keychainApp', 'pid': 456}
    elif changed == 'device':
        phone.device_digest = 'other'
    elif changed == 'build':
        phone.build = 39
    elif changed == 'intent':
        intent = {'outputs': []}
    elif changed == 'session':
        session = 'other'
    else:
        fee = 613
    try:
        with pytest.raises(Blocked):
            phone.resume_first_pin('request', session, intent, fee, journal, 0,
                pin_provider=lambda: pytest.fail('credential must not be requested'), before_pin=lambda: True)
        assert calls == []
    finally:
        journal.close()


@pytest.mark.parametrize('marker', ['pin_resume_probe', 'pin_dispatch_started'])
def test_bound_pin_never_replays_durable_attempt_marker(tmp_path, marker):
    phone, calls = bound_phone()
    journal = bound_journal(tmp_path, phone)
    journal.artifact(marker, {'status': 'started'})
    try:
        with pytest.raises(Blocked) as error:
            phone.resume_first_pin('request', 'session', INTENT, 612, journal, 0,
                pin_provider=lambda: pytest.fail('must not retry'), before_pin=lambda: True)
        assert error.value.code == 'PIN_ATTEMPT_UNCERTAIN'
        assert calls == []
    finally:
        journal.close()


def test_signature_arrival_before_pin_prevents_dispatch(tmp_path):
    phone, calls = bound_phone()
    journal = bound_journal(tmp_path, phone)
    try:
        phone.resume_first_pin('request', 'session', INTENT, 612, journal, 0,
            pin_provider=lambda: pytest.fail('already signed'), before_pin=lambda: False)
        assert calls == []
        assert 'pin_resume_probe' not in journal.data['artifacts']
    finally:
        journal.close()


def test_pin_input_records_intent_before_provider_and_never_retries_lost_click(tmp_path):
    phone, calls = bound_phone()
    journal = bound_journal(tmp_path, phone)
    phone.verify_pin_resume = lambda journal, **kw: journal.artifact('pin_resume_probe', {'status': 'passed'})
    checks = []
    def provider():
        assert journal.data['artifacts']['pin_dispatch_started']['context'] == journal.data['artifacts']['approval_context']
        calls.append('provider')
        return '987654'
    def click(digit):
        calls.append(digit)
        raise RuntimeError('Synthetic lost input response')
    phone.click = click
    try:
        with pytest.raises(RuntimeError):
            phone.resume_first_pin('request', 'session', INTENT, 612, journal, 0,
                pin_provider=provider, before_pin=lambda: checks.append(True) or True)
        assert checks == [True, True]
        assert calls == ['provider', '9']
        with pytest.raises(Blocked):
            phone.resume_first_pin('request', 'session', INTENT, 612, journal, 0,
                pin_provider=provider, before_pin=lambda: True)
        assert calls == ['provider', '9']
        assert '987654' not in journal.path.read_text()
    finally:
        journal.close()


@pytest.mark.parametrize('stopped', [True, False])
def test_upgrade_boundary_requires_verified_stop_before_activation(tmp_path, stopped):
    phone, calls = bound_phone()
    journal = bound_journal(tmp_path, phone)
    receipt = {'original_build': 37, 'current_build': 38, 'phase': 'planned'}
    states = iter([1 if stopped else 4, 4])
    phone.driver.query_app_state = lambda bundle: next(states)
    def activate(bundle):
        assert journal.data['artifacts']['approval_upgrade_recovery']['phase'] == 'process_stopped'
        calls.append('activate')
    phone.driver.activate_app = activate
    try:
        if stopped:
            phone.restart_for_upgrade(journal, receipt)
            assert receipt['phase'] == 'ready_for_review'
            assert receipt['current_process']['pid'] == 123
            assert calls == ['terminate', 'activate']
        else:
            with pytest.raises(Blocked):
                phone.restart_for_upgrade(journal, receipt)
            assert calls == ['terminate']
    finally:
        journal.close()


def test_upgrade_approval_dispatch_consumes_receipt_before_uncertain_click(tmp_path):
    phone, calls = bound_phone()
    journal = bound_journal(tmp_path, phone)
    context = journal.data['artifacts']['approval_context']
    receipt = {'original_build': 37, 'current_build': 38, 'phase': 'ready_for_review',
               'binding': context['binding'], 'current_process': context['process']}
    journal.artifact('approval_upgrade_recovery', receipt)
    phone.restore_navigation = lambda: None
    phone.refresh = lambda: None
    views = iter([{'Testnet4 QA request'}, REVIEW])
    phone.labels = lambda: next(views)
    phone.click_line = calls.append
    def click(label):
        calls.append(label)
        if label == 'Approve':
            assert journal.data['artifacts']['approval_started'] is True
            assert journal.data['artifacts']['approval_upgrade_recovery']['phase'] == 'approval_dispatched'
            raise RuntimeError('Synthetic lost approval response')
    phone.click = click
    try:
        with pytest.raises(RuntimeError):
            phone.approve('request', INTENT, 612, None, journal, 10, session_id='session',
                before_approve=lambda: True, before_pin=lambda: True, pin_provider=lambda: '987654', reapproval=True)
        with pytest.raises(Blocked):
            phone.approve('request', INTENT, 612, None, journal, 10, session_id='session',
                before_approve=lambda: True, before_pin=lambda: True, pin_provider=lambda: '987654', reapproval=True)
        assert calls.count('Approve') == 1
    finally:
        journal.close()


@pytest.mark.parametrize('visible', ['true', 'false'])
def test_account_signer_limit_is_a_prompt_prerequisite_block_without_mutation(visible):
    calls = []
    driver = SimpleNamespace(
        execute_script=lambda command: {'bundleId': 'io.coincube.keychainApp'},
        page_source=f'<root><node visible="{visible}" label="Key limit reached"/></root>',
        find_elements=lambda *a: pytest.fail('must not select controls'),
        activate_app=lambda *a: pytest.fail('must not restart'),
    )
    phone = Phone(driver, 38)
    phone.click = calls.append
    if visible == 'true':
        with pytest.raises(Blocked) as error:
            phone.restore_navigation()
        assert error.value.code == 'SIGNER_LIMIT'
    else:
        assert phone.labels() == set()
    assert calls == []
