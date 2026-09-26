"""Feeds the guard a fake budget message for each threshold.

Run it with `python3 -m unittest` in this directory. It needs no package.
"""

import base64
import json
import unittest

import guard


class Response:
    def __init__(self, status_code, body=None):
        self.status_code = status_code
        self.body = body or {}

    def json(self):
        return self.body

    def raise_for_status(self):
        if self.status_code >= 400:
            raise RuntimeError(f"HTTP {self.status_code}")


class Session:
    """Records each call. The cluster has the pools main and bench."""

    def __init__(self, pools=("main", "bench"), cluster_status=200):
        self.calls = []
        self.pools = pools
        self.cluster_status = cluster_status

    def get(self, url):
        self.calls.append(("GET", url, None))
        return Response(self.cluster_status, {"nodePools": [{"name": name} for name in self.pools]})

    def post(self, url, json):
        self.calls.append(("POST", url, json))
        return Response(200)

    def put(self, url, json):
        self.calls.append(("PUT", url, json))
        return Response(200, {"billingEnabled": False})


def budget(cost, threshold=None):
    message = {
        "budgetDisplayName": "wamn-dev",
        "costAmount": cost,
        "budgetAmount": 50.0,
        "currencyCode": "USD",
    }
    if threshold is not None:
        message["alertThresholdExceeded"] = threshold
    return base64.b64encode(json.dumps(message).encode())


POOL = "https://container.googleapis.com/v1/projects/wamn-dev/locations/us-central1-a/clusters/wamn/nodePools"
SCALED = [
    ("POST", f"{POOL}/main:setSize", {"nodeCount": 0}),
    ("POST", f"{POOL}/bench:setSize", {"nodeCount": 0}),
]
UNLINKED = [
    (
        "PUT",
        "https://cloudbilling.googleapis.com/v1/projects/wamn-dev/billingInfo",
        {"billingAccountName": ""},
    )
]


def run(data, session=None):
    session = session or Session()
    guard.run(guard.decode(data), session, lambda line: None)
    return [call for call in session.calls if call[0] != "GET"]


class Thresholds(unittest.TestCase):
    def test_below_half_does_nothing(self):
        self.assertEqual(run(budget(24.99)), [])

    def test_half_stops_every_pool(self):
        self.assertEqual(run(budget(25.0, 0.5)), SCALED)

    def test_ninety_percent_stops_every_pool(self):
        self.assertEqual(run(budget(45.0, 0.9)), SCALED)

    def test_full_budget_stops_every_pool_then_unlinks_billing(self):
        self.assertEqual(run(budget(50.0, 1.0)), SCALED + UNLINKED)

    def test_the_daily_job_stops_every_pool(self):
        data = base64.b64encode(json.dumps({"action": "scale-to-zero"}).encode())
        self.assertEqual(run(data), SCALED)

    def test_no_cluster_still_unlinks_billing(self):
        self.assertEqual(run(budget(50.0, 1.0), Session(cluster_status=404)), UNLINKED)

    def test_a_failed_scale_still_unlinks_billing_and_reports_it(self):
        session = Session(cluster_status=403)
        with self.assertRaisesRegex(RuntimeError, "scale-to-zero"):
            guard.run(guard.decode(budget(50.0, 1.0)), session, lambda line: None)
        self.assertEqual([call for call in session.calls if call[0] == "PUT"], UNLINKED)

    def test_another_message_is_refused(self):
        data = base64.b64encode(json.dumps({"action": "other"}).encode())
        with self.assertRaises(ValueError):
            run(data)


if __name__ == "__main__":
    unittest.main()
