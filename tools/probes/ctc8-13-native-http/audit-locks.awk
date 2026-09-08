# Usage: awk -f audit-locks.awk ROOT_LOCK PROBE_LOCK
# Read-only package identity audit. Fixture-only additions remain visible.
function emit(    key) {
    if (name == "") return
    key = name SUBSEP version SUBSEP source SUBSEP checksum
    if (file == ARGV[1]) {
        roots[key] = 1
        versions[name] = versions[name] " " version
    } else if (key in roots) {
        matched++
    } else if (name in versions) {
        different++
        print "DIFFERENT " name " root=" versions[name] " probe=" version
    } else {
        added++
        print "ADDED " name " " version
    }
}
function reset() {
    name = version = source = checksum = ""
    file = FILENAME
}
FNR == 1 { emit(); reset() }
/^\[\[package\]\]/ { emit(); reset() }
/^name = / { name = $3; gsub(/"/, "", name) }
/^version = / { version = $3; gsub(/"/, "", version) }
/^source = / { source = $3; gsub(/"/, "", source) }
/^checksum = / { checksum = $3; gsub(/"/, "", checksum) }
END {
    emit()
    print "SUMMARY identical=" matched + 0 " different_shared_names=" different + 0 " new_names=" added + 0
}
