"""Prepare exact grants for the built-in external-capture input composition.

This returns a reviewable host allowlist and updates the candidate manifest's
requested grants. It never writes the running installation or grants trust.
"""
from copy import deepcopy

INPUT = "amaterasu.input"
COORDINATOR = "amaterasu.session-lifecycle"
STREAM = "amaterasu.stream-session"


def policy_value(document, path):
    value = document
    for key in path.split('/')[1:]:
        key = key.replace('~1', '/').replace('~0', '~')
        if not isinstance(value, dict) or key not in value:
            return {"kind": "absent"}
        value = value[key]
    return {"kind": "present", "value": value}


def differences(before, after, path=""):
    if isinstance(before, dict) and isinstance(after, dict):
        result = {}
        for key in sorted(before.keys() | after.keys()):
            child = path + '/' + key.replace('~', '~0').replace('/', '~1')
            if key in before and key in after:
                result.update(differences(before[key], after[key], child))
            else:
                result[child] = {
                    "before": {"kind": "present", "value": before[key]} if key in before else {"kind": "absent"},
                    "after": {"kind": "present", "value": after[key]} if key in after else {"kind": "absent"},
                }
        return result
    return {} if before == after else {path: {"before": {"kind": "present", "value": before}, "after": {"kind": "present", "value": after}}}


def prepare_bound_input(bootstrap, manifest, approved):
    candidate = deepcopy(bootstrap)
    settings = candidate['policy']['settings'][COORDINATOR]
    if settings.get('capture_binding_mode') != 'external':
        raise ValueError('bound-input migration requires external capture')
    if settings.get('input_provider', INPUT) != INPUT:
        raise ValueError('the installation selects another input provider')
    if set(settings['consumers'].get('input', [])) - {INPUT, STREAM}:
        raise ValueError('the installation has independent input consumers')
    components = {component['id']: component for component in manifest['components']}
    for caller, service in [(INPUT, INPUT + '/1'), (COORDINATOR, COORDINATOR + '/1'), (STREAM, STREAM + '/1')]:
        if service not in candidate['policy']['principals'][caller]['provide'] or service not in components[caller]['requested']['provide']:
            raise ValueError('the installation does not select the built-in input composition')
    for caller, service in [(INPUT, COORDINATOR + '/1'), (COORDINATOR, INPUT + '/1')]:
        grants = candidate['policy']['principals'][caller]
        grants['call'] = sorted(set(grants.get('call', [])) | {service})
    settings['input_provider'] = INPUT
    settings['consumers']['input'] = sorted([INPUT, STREAM])
    settings['trusted_callers'] = sorted(set(settings['trusted_callers']) | {INPUT})
    for caller, methods in [(INPUT, ['validate_input_binding']), (STREAM, ['issue_input_binding', 'ack_binding'])]:
        settings['operation_grants'][caller] = sorted(set(settings['operation_grants'].get(caller, [])) | set(methods))
    allowlist = deepcopy(approved)
    for path, change in allowlist['changes'].items():
        if policy_value(bootstrap, path) != change['after']:
            raise ValueError('an earlier policy approval is not fully applied')
    changes = differences(bootstrap, candidate)
    for path in changes:
        if not path.startswith('/policy/'):
            raise ValueError('input migration cannot change bootstrap trust')
        if any(path == old or path.startswith(old + '/') or old.startswith(path + '/') for old in allowlist['changes']):
            raise ValueError('input grants overlap an existing approval')
    allowlist['changes'].update(changes)
    if len(allowlist['changes']) > 256:
        raise ValueError('input migration exceeds the policy change budget')
    for caller, service in [(INPUT, COORDINATOR + '/1'), (COORDINATOR, INPUT + '/1')]:
        grants = components[caller]['requested']
        grants['call'] = sorted(set(grants.get('call', [])) | {service})
    return allowlist
