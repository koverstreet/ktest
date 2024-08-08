@0x94317158fa77dd92;

struct Duration {
    test @0:		Text;
    nr @2:		UInt64;
    passed @3:		UInt64;
    failed @4:		UInt64;
    duration @1:	UInt64;
}

struct Durations {
    entries @0:		List(Duration);
}

# vim: sts=4:sw=4
