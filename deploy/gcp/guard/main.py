"""Cloud Run function entry point of the cost guard. The logic is in guard.py."""

import functions_framework
import google.auth
from google.auth.transport.requests import AuthorizedSession

import guard


@functions_framework.cloud_event
def on_message(event):
    message = guard.decode(event.data["message"]["data"])
    credentials, _ = google.auth.default(scopes=["https://www.googleapis.com/auth/cloud-platform"])
    guard.run(message, AuthorizedSession(credentials), print)
