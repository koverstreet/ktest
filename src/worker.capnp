@0xe7ff10499731ce1f;

struct Worker {
    hostname @0:    Text;
    workdir @1:	    Text;
    starttime @2:   Int64;

    branch @3:	    Text;
    commit @4:	    Text;
    age @5:	    UInt64;
    tests @6:	    Text;
    user @7:	    Text;
}

struct Workers {
    entries @0:	    List(Worker);
}

struct UserStats {
    user @0:	    Text;
    totalSeconds @1:    UInt64;     # all-time runtime
    recentSeconds @2:   Float64;    # time-decayed recent runtime
    lastUpdated @3:	    Int64;      # when recentSeconds was last decayed
}

struct AllUserStats {
    entries @0:	    List(UserStats);
}

# vim: sts=4:sw=4
