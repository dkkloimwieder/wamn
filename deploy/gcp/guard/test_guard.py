"""Feeds the guard replayed budget messages and the daily job.

Run it with `python3 -m unittest` in this directory. It needs no package.
"""

import base64
import json
import unittest

import guard


class Response:
    def __init__(self, status_code, body=None, headers=None):
        self.status_code = status_code
        self.body = body or {}
        self.headers = headers or {}

    def json(self):
        return self.body

    def raise_for_status(self):
        if self.status_code >= 400:
            raise RuntimeError(f"HTTP {self.status_code}")


STATE = "https://storage.googleapis.com/storage/v1/b/wamn-dev-tofu-state/o/guard%2Fstate.json?alt=media"
UPLOAD = "https://storage.googleapis.com/upload/storage/v1/b/wamn-dev-tofu-state/o?uploadType=media&name=guard%2Fstate.json"


class Session:
    """Records each call and holds the state object. The cluster has the pools main and bench."""

    def __init__(self, pools=("main", "bench"), cluster_status=200):
        self.calls = []
        self.pools = pools
        self.cluster_status = cluster_status
        self.state = None
        self.generation = 0

    def get(self, url):
        if url == STATE:
            if self.state is None:
                return Response(404)
            return Response(200, json.loads(self.state), {"x-goog-generation": str(self.generation)})
        self.calls.append(("GET", url, None))
        return Response(self.cluster_status, {"nodePools": [{"name": name} for name in self.pools]})

    def post(self, url, json=None, data=None, headers=None):
        if url.startswith(UPLOAD):
            if url != f"{UPLOAD}&ifGenerationMatch={self.generation}":
                return Response(412)
            self.state = data
            self.generation += 1
            return Response(200)
        self.calls.append(("POST", url, json))
        return Response(200)

    def put(self, url, json):
        self.calls.append(("PUT", url, json))
        return Response(200, {"billingEnabled": False})


OCTOBER = "2026-10-01T07:00:00Z"
NOVEMBER = "2026-11-01T07:00:00Z"


def budget(cost, threshold=None, interval=OCTOBER):
    message = {
        "budgetDisplayName": "wamn-dev",
        "costAmount": cost,
        "costIntervalStart": interval,
        "budgetAmount": 150.0,
        "budgetAmountType": "SPECIFIED_AMOUNT",
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


def run(data, session=None, log=None):
    session = session or Session()
    before = len(session.calls)
    guard.run(guard.decode(data), session, log or (lambda line: None))
    return [call for call in session.calls[before:] if call[0] != "GET"]


class Config(unittest.TestCase):
    def test_the_guard_acts_on_the_configured_project_and_cluster(self):
        self.assertEqual((guard.PROJECT, guard.LOCATION, guard.CLUSTER), ("wamn-dev", "us-central1-a", "wamn"))


class Replay(unittest.TestCase):
    def test_a_month_of_budget_messages_acts_once_per_crossing(self):
        # Billing publishes the current threshold again with every update,
        # about every 20 minutes.
        session = Session()
        lines = []
        replay = [
            (budget(30.0), []),
            (budget(75.0, 0.5), []),
            (budget(80.0, 0.5), []),
            (budget(135.0, 0.9), SCALED),
            (budget(136.0, 0.9), []),
            (budget(140.0, 0.9), []),
            (budget(150.0, 1.0), UNLINKED),
            (budget(151.0, 1.0), []),
            (budget(5.0, None, NOVEMBER), []),
            (budget(136.0, 0.9, NOVEMBER), SCALED),
        ]
        for data, calls in replay:
            self.assertEqual(run(data, session, lines.append), calls)
        self.assertEqual(
            [line for line in lines if line.startswith("budget")],
            ["budget 0.5 crossed: 75.0 of 150.0 USD", "budget 0.9 crossed: 136.0 of 150.0 USD"],
        )
        self.assertEqual(json.loads(session.state), {"interval": NOVEMBER, "threshold": 0.9})

    def test_the_first_message_of_a_month_resets_the_state(self):
        session = Session()
        run(budget(150.0, 1.0), session)
        run(budget(5.0, None, NOVEMBER), session)
        self.assertEqual(json.loads(session.state), {"interval": NOVEMBER, "threshold": 0.0})

    def test_a_jump_past_two_thresholds_runs_both_in_order(self):
        self.assertEqual(run(budget(150.0, 1.0)), SCALED + UNLINKED)

    def test_a_message_below_every_threshold_does_nothing(self):
        self.assertEqual(run(budget(24.99)), [])

    def test_a_failed_action_stores_no_state_so_the_redelivery_acts(self):
        session = Session(cluster_status=403)
        with self.assertRaisesRegex(RuntimeError, "scale-to-zero"):
            run(budget(135.0, 0.9), session)
        self.assertIsNone(session.state)
        session.cluster_status = 200
        self.assertEqual(run(budget(135.0, 0.9), session), SCALED)

    def test_a_concurrent_write_refuses_the_stale_one(self):
        session = Session()
        session.state = json.dumps({"interval": OCTOBER, "threshold": 0.0})
        session.generation = 3
        stale = session.get
        session.get = lambda url: (
            Response(200, {"interval": OCTOBER, "threshold": 0.0}, {"x-goog-generation": "2"}) if url == STATE else stale(url)
        )
        with self.assertRaisesRegex(RuntimeError, "HTTP 412"):
            run(budget(75.0, 0.5), session)

    def test_no_cluster_still_unlinks_billing(self):
        self.assertEqual(run(budget(150.0, 1.0), Session(cluster_status=404)), UNLINKED)

    def test_a_failed_scale_still_unlinks_billing_and_reports_it(self):
        session = Session(cluster_status=403)
        with self.assertRaisesRegex(RuntimeError, "scale-to-zero"):
            run(budget(150.0, 1.0), session)
        self.assertEqual([call for call in session.calls if call[0] == "PUT"], UNLINKED)


class Daily(unittest.TestCase):
    def test_the_daily_job_stops_every_pool_every_time(self):
        session = Session()
        data = base64.b64encode(json.dumps({"action": "scale-to-zero"}).encode())
        self.assertEqual(run(data, session), SCALED)
        self.assertEqual(run(data, session), SCALED)
        self.assertIsNone(session.state)

    def test_another_message_is_refused(self):
        data = base64.b64encode(json.dumps({"action": "other"}).encode())
        with self.assertRaises(ValueError):
            run(data)


if __name__ == "__main__":
    unittest.main()
