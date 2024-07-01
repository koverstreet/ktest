@0xf144e26da5778a14;

struct TestJob {
    branch @0:		Text;
    commit @1:		Text;
    age @2:		UInt64;
    priority @3:	UInt64;
    test @4:		Text;
    subtests @5:	List(Text);
}

struct TestJobs {
    entries @0:		List(TestJob);
}

# vim: sts=4:sw=4
