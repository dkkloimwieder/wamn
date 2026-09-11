The [captured command](command.json) ran the retained terminal helper against the existing `wms_move` binary. It used the host's local terminal and HTTP access. It [passed](record.json) both modes in 4.508828254 seconds, with [empty stderr](stderr).

The [binary record](binary-before.json) identifies 55,967,408 bytes with SHA-256 `7050209feeea17d8149c0a96c8632472ae45177f009e37d3104dfb78e9746d42`. Its exact build commit was not recorded. The [dependency comparison](binary-source-comparison.json) covers all 34 local source files listed by Cargo. Their bytes remain unchanged from `d37f7387` through `8671f63c` and the tested source. This comparison does not establish the missing build record.

[Source capture](source-stability.json) confirms unchanged HEAD, all 28,938 tracked files outside Beads, and binary bytes. The [compressed records](mapping-archives.json) contain complete before and after maps. [publication.json](publication.json) lists every copied original.
