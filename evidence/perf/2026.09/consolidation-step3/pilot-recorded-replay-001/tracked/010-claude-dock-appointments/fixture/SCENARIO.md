# Dock appointments

Carriers book dock appointments. A dock has slots; two appointments on one dock
cannot overlap. Booking is a command with an idempotency key; a replay returns
the same appointment id. An appointment moves scheduled to arrived to departed;
check-in records the actual arrival time. Dispatch needs a list of one dock's
appointments for a day, sortable by slot, filterable by status.

## Invariants

Each invariant carries an id. The exit gate names these ids, so the words below
are the contract and the implementation is yours.

- DOCK-0. Creating a carrier and creating a dock each return an identity the
  later operations address them by.
- DOCK-1. Two appointments on one dock never overlap in time. Whatever else
  happens, the database does not hold an overlapping pair.
- DOCK-2. Booking twice with the same idempotency key and the same request
  returns the same appointment id, and the second call writes nothing new.
- DOCK-3. Booking with an idempotency key already used, and a different
  request, refuses with the typed error `idempotency_conflict`.
- DOCK-4. Check-in moves an appointment from scheduled to arrived and records
  the actual arrival time the caller supplies.
- DOCK-5. Check-in against an appointment that does not exist refuses with the
  typed error `not_found`.
- DOCK-6. Dispatch lists one dock's appointments for one day, filtered by
  status and sorted by slot start, and the order is the slot order.

## Refusals the caller must see

These codes are pinned because the exit gate names them. Any other refusal your
package needs is yours to name.

| code | when |
|---|---|
| `slot_unavailable` | the requested slot overlaps an appointment already on that dock |
| `idempotency_conflict` | the idempotency key was used with a different request |
| `not_found` | the named appointment does not exist |

## Wire names

The exit gate calls these operations by name, so they are pinned. Everything
about how they work is yours.

| operation | what it does |
|---|---|
| `carrier.create` | create a carrier |
| `dock.create` | create a dock |
| `appointment.book` | book a slot on a dock for a carrier; takes an idempotency key |
| `appointment.check_in` | record an arrival against a booked appointment |
| `appointment.query` | list one dock's appointments for one day, filtered and sorted |

## What the words mean here

- A dock is a physical door. It belongs to nothing above it in this scenario.
- A slot is a start time and an end time. Two slots overlap when one starts
  before the other ends and ends after the other starts.
- An appointment joins one carrier to one dock for one slot.
- Status is one of scheduled, arrived or departed. It only moves forward.

There is no reference data. Create carriers and docks through your own
operations before you book anything.
