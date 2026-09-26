"""The cost guard of docs/plan/gcp-deployment.md section 8.2.

It acts on the project wamn-dev and the cluster wamn only. Both are literals,
so no message can point it at another project or at the billing account.
"""

import base64
import json

PROJECT = "wamn-dev"
LOCATION = "us-central1-a"
CLUSTER = "wamn"

CLUSTER_PATH = f"projects/{PROJECT}/locations/{LOCATION}/clusters/{CLUSTER}"
NODE_POOLS = f"https://container.googleapis.com/v1/{CLUSTER_PATH}/nodePools"
BILLING_INFO = f"https://cloudbilling.googleapis.com/v1/projects/{PROJECT}/billingInfo"

SCALE_TO_ZERO = "scale-to-zero"
UNLINK_BILLING = "unlink-billing"


def actions(message):
    """The actions for one decoded Pub/Sub message, in the order to run them.

    A budget message carries costAmount and budgetAmount. The daily job sends
    {"action": "scale-to-zero"}. Anything else is refused.
    """
    if message.get("action") == SCALE_TO_ZERO:
        return [SCALE_TO_ZERO]
    cost = message.get("costAmount")
    budget = message.get("budgetAmount")
    if not isinstance(cost, (int, float)) or not isinstance(budget, (int, float)) or budget <= 0:
        raise ValueError(f"not a budget message or a scale-to-zero message: {message!r}")
    ratio = cost / budget
    if ratio >= 1.0:
        # The pools stop first, because the cluster API refuses calls once
        # billing is off.
        return [SCALE_TO_ZERO, UNLINK_BILLING]
    if ratio >= 0.5:
        return [SCALE_TO_ZERO]
    return []


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


def run(message, session, log):
    """Run every action of the message. One failure does not skip the next."""
    steps = {SCALE_TO_ZERO: scale_to_zero, UNLINK_BILLING: unlink_billing}
    failures = []
    for action in actions(message):
        try:
            steps[action](session, log)
        except Exception as error:  # the next action still runs
            log(f"{action} failed: {error}")
            failures.append(action)
    if failures:
        raise RuntimeError(f"failed: {', '.join(failures)}")
