@0x9527f7d16acca92e;

struct TestResult {
    name @0		:Text;
    duration @1		:UInt64;
    status @2		:Status;
    enum Status {
	inprogress	@0;
	passed		@1;
	failed		@2;
	notrun		@3;
	notstarted	@4;
	unknown		@5;
    }
}

struct TestResults {
    entries @0:		List(TestResult);
}
