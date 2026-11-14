/// The text form of a `pg_lsn`.
pub(super) fn pg_lsn(lsn: u64) -> String {
    format!("{:X}/{:X}", lsn >> 32, lsn & 0xFFFF_FFFF)
}
