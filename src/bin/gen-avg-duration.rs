use std::collections::BTreeMap;
use std::process;
use std::fs::{File, read_to_string};
use ci_cgi::{Ktestrc, ktestrc_read};
use walkdir::WalkDir;

struct TestDuration {
    secs:       u64,
    nr:         u64,
}

type TestDurationMap = BTreeMap<String, TestDuration>;

fn read_duration_sums(rc: &Ktestrc) -> TestDurationMap {
    let mut durations: BTreeMap<String, TestDuration> = BTreeMap::new();

    for i in WalkDir::new(&rc.output_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() == "duration") {
        let i = i.into_path();

        let duration: Option<u64> = read_to_string(&i).ok()
            .map(|d| d.parse().ok())
            .flatten();
        if duration.is_none() {
            continue;
        }
        let duration = duration.unwrap();
        let test = i.components().nth_back(1);
        if test.is_none() {
            continue;
        }
        let test = test.unwrap().as_os_str().to_string_lossy().to_string();

        let t = durations.get_mut(&test);
        if let Some(t) = t {
            t.secs  += duration;
            t.nr    += 1;
        } else {
            durations.insert(test, TestDuration { secs: duration, nr: 1 });
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

