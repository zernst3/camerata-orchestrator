# clean_repo

Integration fixture for `camerata-check`: a fully compliant repo (RLS enabled + a real policy
on its one table) with no architectural violations for any registered checker. Used to prove
the binary exits `0` and reports `"clean": true` on a repo with nothing to flag.
