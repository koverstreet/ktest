@0xb8e4f9a1c3d27e56;

struct BranchEntry {
    commitId @0:	Text;
    message @1:		Text;
    passed @2:		UInt32;
    failed @3:		UInt32;
    notrun @4:		UInt32;
    notstarted @5:	UInt32;
    inprogress @6:	UInt32;
    unknown @7:		UInt32;
    duration @8:	UInt64;
}

struct BranchLog {
    entries @0:		List(BranchEntry);
}

# vim: sts=4:sw=4
