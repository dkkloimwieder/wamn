"""The cost guard of docs/history/gcp-deployment.md section 8.2.

It acts on the one project and the one cluster that config.json names, and
never on the billing account. A message cannot name another project.
"""

import base64
import json
from pathlib import Path
from urllib.parse import quote

CONFIG = json.loads(Path(__file__).with_name("config.json").read_text())
PROJECT = CONFIG["project"]
LOCATION = CONFIG["location"]
CLUSTER = CONFIG["cluster"]

CLUSTER_PATH = f"projects/{PROJECT}/locations/{LOCATION}/clusters/{CLUSTER}"
NODE_POOLS = f"https://container.googleapis.com/v1/{CLUSTER_PATH}/nodePools"
BILLING_INFO = f"https://cloudbilling.googleapis.com/v1/projects/{PROJECT}/billingInfo"
STATE_READ = (
    f"https://storage.googleapis.com/storage/v1/b/{CONFIG['state_bucket']}"
    f"/o/{quote(CONFIG['state_object'], safe='')}?alt=media"
)
STATE_WRITE = (
    f"https://storage.googleapis.com/upload/storage/v1/b/{CONFIG['state_bucket']}"
    f"/o?uploadType=media&name={quote(CONFIG['state_object'], safe='')}"
)

NOTIFY = "notify"
SCALE_TO_ZERO = "scale-to-zero"
UNLINK_BILLING = "unlink-billing"

# The action of each budget threshold, in the order to run them. The pools
# stop before the unlink, because the cluster API refuses calls once billing
# is off.
THRESHOLDS = [(0.5, NOTIFY), (0.9, SCALE_TO_ZERO), (1.0, UNLINK_BILLING)]


def actions(message, stored):
    """The actions of one budget message, in the order to run them.

    A budget message acts once for each threshold above `stored`, the last
    alertThresholdExceeded of its cost interval, up to its own
    alertThresholdExceeded. A message without one crossed no threshold.
    """
    exceeded = message.get("alertThresholdExceeded", 0.0)
    return [action for threshold, action in THRESHOLDS if stored < threshold <= exceeded]


def read_state(session):
    """The stored state and its generation. A missing object is generation 0."""
    response = session.get(STATE_READ)
    if response.status_code == 404:
        return {}, 0
    response.raise_for_status()
    return response.json(), int(response.headers["x-goog-generation"])


def write_state(session, state, generation):
    """Replace the state only if no other message wrote it since the read."""
    response = session.post(
        f"{STATE_WRITE}&ifGenerationMatch={generation}",
        data=json.dumps(state),
        headers={"Content-Type": "application/json"},
    )
    response.raise_for_status()


def decode(data):
    """The JSON of a Pub/Sub message body, base64 encoded."""
    return json.loads(base64.b64decode(data))


def scale_to_zero(session, log):
    """Set every node pool of the cluster to 0 nodes. No cluster is no work."""
    response = session.get(NODE_POOLS)
    if response.status_code == 404:
        log(f"{CLUSTER_PATH} does not exist, so no pool runs")
        return
    response.raise_for_status()
    for pool in response.json().get("nodePools", []):
        name = pool["name"]
        result = session.post(f"{NODE_POOLS}/{name}:setSize", json={"nodeCount": 0})
        result.raise_for_status()
        log(f"{CLUSTER_PATH}/nodePools/{name} set to 0 nodes")


def unlink_billing(session, log):
    """Remove the billing account from the project."""
    response = session.put(BILLING_INFO, json={"billingAccountName": ""})
    response.raise_for_status()
    log(f"billing unlinked from {PROJECT}: {response.json()}")


def perform(steps, session, log):
    """Run every action. One failure does not skip the next."""
    failures = []
    for action, step in steps:
        try:
            step(session, log)
        except Exception as error:  # the next action still runs
            log(f"{action} failed: {error}")
            failures.append(action)
    if failures:
        raise RuntimeError(f"failed: {', '.join(failures)}")


def run(message, session, log):
    """Run the actions of the message, then store the threshold it reached.

    The daily job sends {"action": "scale-to-zero"} and reads no state. A
    budget message of a new cost interval resets the stored threshold to 0.
    The state is stored only after every action succeeded, so Pub/Sub
    delivers a failed message again. Anything else is refused.
    """
    if message.get("action") == SCALE_TO_ZERO:
        perform([(SCALE_TO_ZERO, scale_to_zero)], session, log)
        return
    interval = message.get("costIntervalStart")
    if not isinstance(interval, str):
        raise ValueError(f"not a budget message or a scale-to-zero message: {message!r}")
    state, generation = read_state(session)
    stored = state.get("threshold", 0.0) if state.get("interval") == interval else 0.0

    def notify(_session, log):
        log(f"budget {message.get('alertThresholdExceeded')} crossed: "
            f"{message.get('costAmount')} of {message.get('budgetAmount')} {message.get('currencyCode')}")

    steps = {NOTIFY: notify, SCALE_TO_ZERO: scale_to_zero, UNLINK_BILLING: unlink_billing}
    perform([(action, steps[action]) for action in actions(message, stored)], session, log)
    exceeded = message.get("alertThresholdExceeded", 0.0)
    if state.get("interval") != interval or exceeded > stored:
        write_state(session, {"interval": interval, "threshold": exceeded}, generation)
