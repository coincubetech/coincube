#!/usr/bin/env python3
"""Read public Testnet4 chain evidence from two Esplora providers."""
import argparse
import json
import re
import sys
from urllib.error import URLError
from urllib.request import HTTPRedirectHandler, build_opener

GENESIS = "00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043"
ENDPOINTS = (
    "https://mempool.space/testnet4/api",
    "https://blockstream.info/testnet4/api",
)
MAX_RESPONSE_BYTES = 1_000_000


class NoRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


def read(opener, endpoint, path):
    with opener.open(endpoint + path, timeout=10) as response:
        payload = response.read(MAX_RESPONSE_BYTES + 1)
    if len(payload) > MAX_RESPONSE_BYTES:
        raise ValueError("response exceeds size limit")
    return payload.decode("utf-8").strip()


def probe(opener, endpoint, txid):
    if read(opener, endpoint, "/block-height/0") != GENESIS:
        raise ValueError("Bitcoin Testnet4 genesis mismatch")
    tip = int(read(opener, endpoint, "/blocks/tip/height"))
    if tip < 1:
        raise ValueError("invalid chain tip")
    evidence = {"endpoint": endpoint, "genesis": GENESIS, "tip": tip}
    if txid is None:
        return evidence

    tx = json.loads(read(opener, endpoint, f"/tx/{txid}"))
    if tx["txid"] != txid:
        raise ValueError("transaction ID mismatch")
    status = tx["status"]
    confirmed = status["confirmed"]
    if type(confirmed) is not bool:
        raise ValueError("invalid confirmation state")
    height = status["block_height"] if confirmed else None
    if confirmed and (type(height) is not int or height < 1):
        raise ValueError("invalid confirmation height")
    fee = tx["fee"]
    if type(fee) is not int or fee < 0:
        raise ValueError("invalid fee")
    evidence["transaction"] = {
        "txid": txid,
        "fee_sats": fee,
        "confirmed": confirmed,
        "block_height": height,
        "block_hash": status["block_hash"] if confirmed else None,
    }
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--txid", help="Optional public transaction ID to query")
    args = parser.parse_args()
    if args.txid and not re.fullmatch(r"[a-fA-F0-9]{64}", args.txid):
        parser.error("--txid must be 64 hexadecimal characters")
    txid = args.txid.lower() if args.txid else None

    opener = build_opener(NoRedirects)
    evidence = []
    try:
        for endpoint in ENDPOINTS:
            evidence.append(probe(opener, endpoint, txid))
        if txid and evidence[0]["transaction"] != evidence[1]["transaction"]:
            raise ValueError("providers disagree on transaction fee or confirmation state")
    except (URLError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"Testnet4 verification failed: {type(error).__name__}", file=sys.stderr)
        return 1
    print(json.dumps(evidence, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
