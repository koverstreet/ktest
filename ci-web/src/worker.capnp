@0xe7ff10499731ce1f;

struct Worker {
    hostname @0:    Text;
    workdir @1:	    Text;
    starttime @2:   Int64;

    branch @3:	    Text;
    commit @4:	    Text;
    age @5:	    UInt64;
    tests @6:	    Text;
}

struct Workers {
    entries @0:	    List(Worker);
}

# vim: sts=4:sw=4
