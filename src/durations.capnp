@0x94317158fa77dd92;

struct Duration {
    test @0:		Text;
    duration @1:	UInt64;
}

struct Durations {
    entries @0:	    List(Duration);
}

# vim: sts=4:sw=4
