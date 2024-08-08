use std::collections::BTreeMap;
use std::process;
use std::fs::File;
use ci_cgi::{Ktestrc, ktestrc_read, commitdir_get_results, TestStatus};

#[derive(Default, Debug)]
struct TestDuration {
    secs:       u64,
    nr:         u64,
    nr_passed:  u64,
    nr_failed:  u64,
}

type TestDurationMap = BTreeMap<String, TestDuration>;

fn read_duration_sums(rc: &Ktestrc) -> TestDurationMap {
    let mut durations: BTreeMap<String, TestDuration> = BTreeMap::new();

    for i in std::fs::read_dir(&rc.output_dir).unwrap()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.metadata().unwrap().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|e| commitdir_get_results(rc, &e).ok()) {

        for (test, result) in i {
            let mut t = durations.get_mut(&test);

            if t.is_none() {
                durations.insert(test.clone(), Default::default());
                t = durations.get_mut(&test);
            }
            let t = t.unwrap();

            t.secs      += result.duration;
            t.nr        += 1;
            t.nr_passed += (result.status == TestStatus::Passed) as u64;
            t.nr_failed += (result.status == TestStatus::Failed) as u64;
        }
    }

    durations
}

fn write_durations_capnp(rc: &Ktestrc, durations_in: TestDurationMap) {
    use ci_cgi::durations_capnp::durations;
    use capnp::serialize;

    let mut message = capnp::message::Builder::new_default();
    let root: durations::Builder = message.init_root();
    let mut entries = root.init_entries(durations_in.len().try_into().unwrap());

    for (idx, (name, duration_in)) in durations_in.iter().enumerate() {
        let mut duration_out = entries.reborrow().get(idx.try_into().unwrap());

        duration_out.set_test(name);
        duration_out.set_nr(duration_in.nr);
        duration_out.set_passed(duration_in.nr_passed);
        duration_out.set_failed(duration_in.nr_failed);
        duration_out.set_duration(duration_in.secs / duration_in.nr);
    }

    let fname       = rc.output_dir.join("test_durations.capnp");
    let fname_new   = rc.output_dir.join("test_durations.capnp.new");

    let mut out = File::create(&fname_new).unwrap();

    serialize::write_message(&mut out, &message).unwrap();
    drop(out);
    std::fs::rename(fname_new, &fname).unwrap();

    println!("wrote durations for {} tests to {}", durations_in.len(), fname.display());
}

fn main() {
    let ktestrc = ktestrc_read();
    if let Err(e) = ktestrc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let ktestrc = ktestrc.unwrap();

    let durations = read_duration_sums(&ktestrc);
    write_durations_capnp(&ktestrc, durations);
}

