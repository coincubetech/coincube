"""Encrypted macOS Keychain records through the signed native helper."""
import json

from .state import Failed, atomic_write, private_read


class SecretStore:
    def __init__(self, provision, locator=None):
        self.provision = provision
        self.sha256 = None
        self.locator = locator

    def load(self):
        reply = self.provision.call("secret_get", secret_name="phone_credentials")
        self.sha256 = reply["sha256"]
        value = reply["value"]
        if value is not None and not isinstance(value, dict):
            raise Failed("SECRET_PROTOCOL", "The encrypted credential record has an invalid format.")
        if self.locator is not None:
            address = {k: reply[k] for k in ("service", "account")}
            if self.locator.exists():
                if json.loads(private_read(self.locator)) != address or value is None:
                    raise Failed("SECRET_MISSING", "The saved encrypted credential record is missing or changed; it will not be replaced.")
            elif value is not None:
                atomic_write(self.locator, json.dumps(address).encode())
        return value

    def save(self, value):
        reply = self.provision.call("secret_put", secret_name="phone_credentials", secret_value=value,
                                    expected_previous_sha256=self.sha256)
        expected = reply["sha256"]
        if self.load() != value or self.sha256 != expected:
            raise Failed("SECRET_READBACK", "Encrypted credential readback did not match; credentials were not used.")
