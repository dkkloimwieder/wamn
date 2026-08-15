FROM --platform=linux/amd64 postgres:18.6-trixie@sha256:ae6c78831cbc35fa3a4aaf4d763ddacf6183d6004774cc2dc28b3920410d1d1a

LABEL wamn.dev/gate="m1-check-9" \
      wamn.dev/upstream-index="sha256:ae6c78831cbc35fa3a4aaf4d763ddacf6183d6004774cc2dc28b3920410d1d1a" \
      wamn.dev/upstream-child="sha256:cd78ca58eb75f929698e117a589488ccb2bd45107247fe02400b50ff6c418324"
