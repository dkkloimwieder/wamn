# Receiving operator guide

Receiving records delivered quantities against a purchase order.
You choose an order, enter quantities, choose a location, and send one receipt.
Your administrator supplies the launch command and access to the required environment.
An environment is the application workspace that you access.

## Sign in

At sign-in prompts, type your answer and press Enter.
Inside Receiving, navigation keys act immediately.

For your first login:

1. Open the invitation email from WAMN.
2. Start Receiving with the command from your administrator.
3. Enter `I` at the sign-in menu.
4. Paste the complete invitation code from the email.
5. Enter a new password with at least 15 characters.
6. Enter the same password again.
7. Use the normal login prompts to enter your email and password.

The invitation code expires and works once.
Use the complete code as one value.
You do not need a PAT, which is a separately issued access credential.
An invitation establishes your password. Your administrator assigns your access separately.

For later visits, enter `L`, then your email and password.
One available environment opens directly. With several environments, enter the number of your choice.
If no environment is available, contact your administrator.

Email addresses and menu choices remain visible. Passwords and email codes show masking characters.
An empty Enter does not advance a prompt.
Esc or Ctrl-C cancels a sign-in prompt.

## Record a delivery

Enter saves the field that you edit. It does not send the receipt.
Ctrl-S sends the completed receipt to the application.

1. In the order list, use Up or Down to select the purchase order.
2. Press Enter to open its receipt lines.
3. Wait for the lines and locations to load.
4. Use Up or Down to select a line.
5. Press Enter to edit its delivered quantity.
6. Type the quantity, then press Enter to save the field.
7. For another line, press F2 and repeat the quantity steps.
8. Press F3 to edit the receipt reference.
9. Enter your delivery document reference, then press Enter.
10. Press F4 to open locations.
11. Use Up or Down to select the receiving location.
12. Press Enter to choose that location.
13. Press F9 to view the receipt inputs.
14. Make sure that the reference, quantities, and location are correct.
15. Press Ctrl-S to send the receipt.
16. Wait for `Recorded receipt` and the receipt identifier.

The first loaded location is selected initially. Your location choice applies to every line in this receipt.
You cannot assign different locations to individual lines through this workflow.

Only lines with a quantity enter the receipt. Leave other quantities empty.
For example, enter `4.0000` to receive four units.
Receiving refuses quantities above the remaining ordered quantity.
Another delivery can change that remaining quantity while your screen stays open.

The field editor starts with the existing value. Backspace removes characters from its end.
Esc cancels the field edit. It keeps the previous value.
While a field editor is open, finish or cancel it before using navigation shortcuts.

Success shows the committed receipt identifier, purchase order status, and revision immediately.
Press `d` for optional details or `h` for purchase order history.
A failed optional read does not undo the receipt or send it again.
Press Esc from either read to return to the committed result.
Press Esc again to return to the order list.
Press F5 to reload the order list before selecting the next order.
To refresh lines inside an open receipt, leave the receipt and reopen the order.

## Keyboard reference

These shortcuts apply outside a field editor or confirmation prompt.
Function keys require a terminal that passes those keys to Receiving.

| Key | Action |
| --- | --- |
| Up / Down | Select a row or receipt input |
| Left / Right | Select a result column |
| Tab | Switch between inputs and results on a generated screen |
| Enter in the order list | Open the selected order for receiving |
| Enter on a receipt line | Edit its quantity |
| Enter in the location list | Choose the location for all receipt lines |
| F2 | Return to the open order's lines |
| F3 | Edit the receipt reference |
| F4 | Open the location list |
| `l` | Cycle through loaded locations for the open receipt |
| F9 | Show the receipt inputs |
| Ctrl-S | Send the open receipt |
| F5 in the order list | Refresh the order list |
| F8 in the order list | Load the next available page |
| `h` in the order list | Open the selected purchase order's history |
| `d` after success | Read optional receipt details or Acme quality details |
| `h` after success | Read the committed purchase order's history |
| Esc | Leave the current view or request draft cancellation |
| `q` | Quit, with a confirmation when required |

Some shared screen help lists F6 for a new command.
Receiving requires you to leave the current receipt and open an order instead.
The order list supports forward pagination. F5 refreshes from the first page.

## Submission results and cancellation

A draft is receipt data that you have not sent.
Before submission, Esc requests that you leave the receipt.
At the discard prompt, press `y` to discard the draft or `n` to keep it.
These confirmation keys do not need Enter. Esc also dismisses the confirmation.

While a request is pending, wait for its result.
Quitting the client does not cancel server work.
Do not assume that closing the terminal reverses a delivery.

Acme uses the same entry screens and sends its receipt through the Acme operation.
Acme applies its inspection policy before the receipt commits.
A QC refusal appears as `Refused`, separately from a committed receipt or an unknown outcome.

If the screen reports `Refused`, read the reason before changing the receipt.
Correct the relevant input or contact your administrator for an access refusal.
If the quantities are stale, leave the receipt and reopen the order to load current lines.

If the screen reports `Outcome unknown`, the request can still complete.
Do not create another receipt for the same delivery merely because the response is missing.
Follow the recovery instruction on the screen.
When it offers a retry of the captured command, press F9, then F7.
A captured command is the exact request that Receiving previously sent.
This retry preserves that request instead of creating a new delivery.
The direct Receiving route supports captured retry. The Acme route does not offer this retry.
Follow the route's recovery instruction. Do not repeat the whole command automatically.
If retry is unavailable or refused, ask your administrator to establish the result before resubmitting.

If the screen reports `Partially completed`, committed work remains.
Do not repeat the whole delivery without establishing what completed.
Leaving an uncertain request discards the local retry information. It does not stop that request.
Drafts and retry information do not survive quitting the terminal.

## Purchase order history

In the order list, select an order and press `h`.
Receiving loads its history and initially selects the latest entry.
Use Up or Down to select an earlier or later entry.
Press Esc to return to the order list.

Each entry shows its position, change kind, operation, actor identifier, and time.
The state below the entries shows the purchase order at the selected position.
This view shows purchase order history. It is not a list of all receipt lines or a general audit search.

## Password recovery and logout

To replace a forgotten password:

1. Start Receiving and enter `R` at the sign-in menu.
2. Enter your account email.
3. Wait for the reset-code prompt and the recovery email.
4. Paste the emailed reset secret, then press Enter.
5. Enter a new password with at least 15 characters.
6. Enter the same password again.
7. Enter `L` when prompted, then sign in with your email and new password.

The reset secret expires after 15 minutes and works once.
The request response does not disclose whether an email belongs to an eligible account.
If no email arrives, contact your administrator.
If Receiving reports a notification delivery failure after changing the password, the new password still applies.

If you lose access to your mailbox, contact your administrator.
An administrator must follow the [mailbox-loss recovery procedure](../../docs/operations/deployment.md#mailbox-loss-recovery).
Receiving has no self-service email change or alternative trusted-contact channel.

Receiving keeps login credentials only while the program runs.
It renews access when a request needs it. An idle terminal does not renew access in the background.
If renewal fails or your login expires, exit and sign in again.
Then explicitly submit your intended operation. Receiving does not automatically repeat an application operation.
For an unknown earlier result, follow the submission guidance above before resubmitting.

Outside a field editor, press `q` to quit and log out.
If a discard prompt appears, press `y` to proceed.
Logout clears local credentials and asks the server to stop further renewal.
After server logout succeeds, existing login credentials cannot start another application request.
If server logout cannot be confirmed, Receiving reports that limitation.

## Administrator and developer setup

The [development loop](../../docs/operations/development-loop.md#receiving-password-login) documents launch configuration and identity connections.
The [Receiving scenario](receiving-scenario.md) explains application limits and source ownership.
