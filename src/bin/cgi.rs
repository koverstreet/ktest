use std::collections::BTreeMap;
use std::fmt::Write;
use regex::Regex;
use chrono::Duration;
extern crate cgi;
extern crate querystring;

use ci_cgi::{CiConfig, Userrc, ciconfig_read, TestResultsMap, TestStatus, commitdir_get_results, git_get_commit, workers_get, update_lcov};

const COMMIT_FILTER:    &str = include_str!("../../commit-filter");
const STYLESHEET:       &str = "bootstrap.min.css";

const COMMIT_CSS_JS:       &str =
"
<style>
.toplevel-container {
        margin-left: 10px;
}
.horizontal {
        margin-right: 10px;
}
.horizontal-container {
        display: flex;
        flex-direction: row;
}
</style>
<script>
document.getElementById('myLink').addEventListener('click', function(event) {
    event.preventDefault(); // Prevents the default link behavior
    alert('Link clicked!');
});
</script>
";

fn filter_results(r: TestResultsMap, tests_matching: &Regex) -> TestResultsMap {
    r.iter()
        .filter(|i| tests_matching.is_match(&i.0) )
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

struct Ci {
    rc:                 CiConfig,
    repo:               git2::Repository,
    stylesheet:         String,
    script_name:        String,

    user:               Option<String>,
    branch:             Option<String>,
    commit:             Option<String>,
    tests_matching:     Regex,
}

struct CommitResults {
    id:             String,
    message:        String,
    tests:          TestResultsMap,
}

fn branch_get_results(ci: &Ci) -> Result<Vec<CommitResults>, String> {
    fn commit_get_results(ci: &Ci, commit: &git2::Commit) -> CommitResults {
        let id = commit.id().to_string();
        let tests = commitdir_get_results(&ci.rc.ktest, &id).unwrap_or(BTreeMap::new());
        let tests = filter_results(tests, &ci.tests_matching);

        CommitResults {
            id,
            message:    commit.message().unwrap().to_string(),
            tests,
        }
    }

    let mut nr_empty = 0;
    let mut nr_commits = 0;
    let mut ret: Vec<CommitResults> = Vec::new();

    let branch_or_commit =
        ci.commit.as_ref().or(ci.branch.as_ref()).unwrap();
    let mut walk = ci.repo.revwalk().unwrap();

    let reference = git_get_commit(&ci.repo, branch_or_commit.clone());
    if reference.is_err() {
        /* XXX: return a 404 */
        return Err(format!("commit not found"));
    }
    let reference = reference.unwrap();

    if let Err(e) = walk.push(reference.id()) {
        return Err(format!("Error walking {}: {}", branch_or_commit, e));
    }

    for commit in walk
            .filter_map(|i| i.ok())
            .filter_map(|i| ci.repo.find_commit(i).ok()) {
        let r = commit_get_results(ci, &commit);

        if !r.tests.is_empty() {
            nr_empty = 0;
        } else {
            nr_empty += 1;
            if nr_empty > 100 {
                break;
            }
        }

        ret.push(r);

        nr_commits += 1;
        if nr_commits > 50 {
            break;
        }
    }

    
    while !ret.is_empty() && ret[ret.len() - 1].tests.is_empty() {
        ret.pop();
    }

    Ok(ret)
}

fn ci_log(ci: &Ci) -> cgi::Response {
    let mut out = String::new();
    let branch = ci.branch.as_ref().unwrap();

    let commits = branch_get_results(ci);
    if let Err(e) = commits {
        return error_response(e);
    }

    let commits = commits.unwrap();

    let mut multiple_test_view = false;
    for r in &commits {
        if r.tests.len() > 1 {
            multiple_test_view = true;
        }
    }

    writeln!(&mut out, "<!DOCTYPE HTML>").unwrap();
    writeln!(&mut out, "<html><head><title>{}</title></head>", branch).unwrap();
    writeln!(&mut out, "<link href=\"{}\" rel=\"stylesheet\">", ci.stylesheet).unwrap();

    writeln!(&mut out, "<body>").unwrap();
    writeln!(&mut out, "<div class=\"container\">").unwrap();
    writeln!(&mut out, "<table class=\"table\">").unwrap();


    if multiple_test_view {
        writeln!(&mut out, "<tr>").unwrap();
        writeln!(&mut out, "<th> Commit      </th>").unwrap();
        writeln!(&mut out, "<th> Description </th>").unwrap();
        writeln!(&mut out, "<th> Passed      </th>").unwrap();
        writeln!(&mut out, "<th> Failed      </th>").unwrap();
        writeln!(&mut out, "<th> Not started </th>").unwrap();
        writeln!(&mut out, "<th> Not run     </th>").unwrap();
        writeln!(&mut out, "<th> In progress </th>").unwrap();
        writeln!(&mut out, "<th> Unknown     </th>").unwrap();
        writeln!(&mut out, "<th> Total       </th>").unwrap();
        writeln!(&mut out, "<th> Duration    </th>").unwrap();
        writeln!(&mut out, "</tr>").unwrap();

        let mut nr_empty = 0;
        for r in &commits {
            if !r.tests.is_empty() {
                if nr_empty != 0 {
                    writeln!(&mut out, "<tr> <td> ({} untested commits) </td> </tr>", nr_empty).unwrap();
                    nr_empty = 0;
                }

                fn count(r: &TestResultsMap, t: TestStatus) -> usize {
                    r.iter().filter(|x| x.1.status == t).count()
                }

                let subject_len = r.message.find('\n').unwrap_or(r.message.len());

                let duration: u64 = r.tests.iter().map(|x| x.1.duration).sum();

                writeln!(&mut out, "<tr>").unwrap();
                writeln!(&mut out, "<td> <a href=\"{}?branch={}&commit={}\">{}</a> </td>",
                         ci.script_name, branch,
                         r.id, &r.id.as_str()[..14]).unwrap();
                writeln!(&mut out, "<td> {} </td>", &r.message[..subject_len]).unwrap();
                writeln!(&mut out, "<td> {} </td>", count(&r.tests, TestStatus::Passed)).unwrap();
                writeln!(&mut out, "<td> {} </td>", count(&r.tests, TestStatus::Failed)).unwrap();
                writeln!(&mut out, "<td> {} </td>", count(&r.tests, TestStatus::Notstarted)).unwrap();
                writeln!(&mut out, "<td> {} </td>", count(&r.tests, TestStatus::Notrun)).unwrap();
                writeln!(&mut out, "<td> {} </td>", count(&r.tests, TestStatus::Inprogress)).unwrap();
                writeln!(&mut out, "<td> {} </td>", count(&r.tests, TestStatus::Unknown)).unwrap();
                writeln!(&mut out, "<td> {} </td>", r.tests.len()).unwrap();
                writeln!(&mut out, "<td> {}s </td>", duration).unwrap();
                writeln!(&mut out, "</tr>").unwrap();
            } else {
                nr_empty += 1;
            }
        }
    } else {
        writeln!(&mut out, "<tr>").unwrap();
        writeln!(&mut out, "<th> Commit      </th>").unwrap();
        writeln!(&mut out, "<th> Description </th>").unwrap();
        writeln!(&mut out, "<th> Status      </th>").unwrap();
        writeln!(&mut out, "<th> Duration    </th>").unwrap();
        writeln!(&mut out, "</tr>").unwrap();

        let mut nr_empty = 0;
        for r in &commits {
            if let Some(t) = r.tests.first_key_value() {
                if nr_empty != 0 {
                    writeln!(&mut out, "<tr> <td> ({} untested commits) </td> </tr>", nr_empty).unwrap();
                    nr_empty = 0;
                }

                let subject_len = r.message.find('\n').unwrap_or(r.message.len());

                writeln!(&mut out, "<tr class={}>", t.1.status.table_class()).unwrap();
                writeln!(&mut out, "<td> <a href=\"{}?branch={}&commit={}\">{}</a> </td>",
                         ci.script_name, branch,
                         r.id, &r.id.as_str()[..14]).unwrap();
                writeln!(&mut out, "<td> {} </td>", &r.message[..subject_len]).unwrap();
                writeln!(&mut out, "<td> {} </td>", t.1.status.to_str()).unwrap();
                writeln!(&mut out, "<td> {}s </td>", t.1.duration).unwrap();
                writeln!(&mut out, "<td> <a href=c/{}/{}/log.br>        log                 </a> </td>", &r.id, t.0).unwrap();
                writeln!(&mut out, "<td> <a href=c/{}/{}/full_log.br>   full log            </a> </td>", &r.id, t.0).unwrap();
                writeln!(&mut out, "<td> <a href=c/{}/{}>		        output directory    </a> </td>", &r.id, t.0).unwrap();
                writeln!(&mut out, "</tr>").unwrap();
            } else {
                nr_empty += 1;
            }
        }
    }

    writeln!(&mut out, "</table>").unwrap();
    writeln!(&mut out, "</div>").unwrap();
    writeln!(&mut out, "</body>").unwrap();
    writeln!(&mut out, "</html>").unwrap();
    cgi::html_response(200, out)
}

fn last_good_line(results: &Vec<CommitResults>, test: &str) -> String {
    for (idx, result) in results.iter().map(|i| i.tests.get(test)).enumerate() {
        if let Some(result) = result {
            if result.status == TestStatus::Passed {
                return format!("{}", idx);
            }

            if result.status != TestStatus::Failed {
                return format!("&gt;= {}", idx);
            }
        } else {
            return format!("&gt;= {}", idx);
        }
    }

    return format!("&gt;= {}", results.len());
}

fn log_link(out: &mut String, fname: &str, link: &str) {
    let onclick = format!(
"fetch('{}')
    .then(response => response.text())
    .then(text => {{
        document.getElementById('testlog').textContent = text;
  }});
  return false;
", &fname);

    writeln!(out, "<td> <a href={} onclick=\"{}\"> {} </a> </td>", fname, onclick, link).unwrap();
}

fn ci_commit(ci: &Ci) -> cgi::Response {
    let mut out = String::new();

    let commits = branch_get_results(ci);
    if let Err(e) = commits {
        return error_response(e);
    }
    let commits = commits.unwrap();

    let first_commit = &commits[0];
    let message = &first_commit.message;
    let subject_len = message.find('\n').unwrap_or(message.len());

    writeln!(&mut out, "<!DOCTYPE HTML>").unwrap();
    writeln!(&mut out, "<html><head><title>{}</title></head>", &message[..subject_len]).unwrap();
    writeln!(&mut out, "<link href=\"{}\" rel=\"stylesheet\">", ci.stylesheet).unwrap();

    writeln!(&mut out, "<body>").unwrap();
    writeln!(&mut out, "<div class=\"toplevel-container\">").unwrap();

    writeln!(&mut out, "<h3><th>{}</th></h3>", &message[..subject_len]).unwrap();

    update_lcov(&ci.rc.ktest, &first_commit.id);

    if ci.rc.ktest.output_dir.join(&first_commit.id).join("lcov").exists() {
        writeln!(&mut out, "<p> <a href=c/{}/lcov> Code coverage </a> </p>", &first_commit.id).unwrap();
    }

    out.push_str(COMMIT_FILTER);
    out.push_str(COMMIT_CSS_JS);

    writeln!(&mut out, "<div class=\"horizontal-container\">").unwrap();

    writeln!(&mut out, "<div class=\"horizontal\">").unwrap();
    writeln!(&mut out, "<table class=\"table\">").unwrap();
    for (name, result) in &first_commit.tests {
        writeln!(&mut out, "<tr class={}>", result.status.table_class()).unwrap();
        log_link(&mut out, &format!("c/{}/{}/log.br", &first_commit.id, name), name);
        writeln!(&mut out, "<td> {}s </td>", result.duration).unwrap();
        writeln!(&mut out, "<td> {}  </td>", result.status.to_str()).unwrap();
        writeln!(&mut out, "<td> {}  </td>", last_good_line(&commits, name)).unwrap();
        if let Some(branch) = &ci.branch {
            writeln!(&mut out, "<td> <a href={}?branch={}&test=^{}$> git log        </a> </td>",
                     ci.script_name, &branch, name).unwrap();
        }

        log_link(&mut out, &format!("c/{}/{}/full_log.br", &first_commit.id, name), "full");

        /*  We're not currently using this:
        writeln!(&mut out, "<td> <a href=c/{}/{}>		        output directory    </a> </td>", &first_commit.id, name).unwrap();
        */

        writeln!(&mut out, "</tr>").unwrap();
    }
    writeln!(&mut out, "</table>").unwrap();
    writeln!(&mut out, "</div>").unwrap();

    writeln!(&mut out, "<div class=\"horizontal\">").unwrap();
    writeln!(&mut out, " <pre id=\"testlog\"></pre> ").unwrap();
    writeln!(&mut out, "</div>").unwrap();

    writeln!(&mut out, "</div>").unwrap();
    writeln!(&mut out, "</body>").unwrap();
    writeln!(&mut out, "</html>").unwrap();
    cgi::html_response(200, out)
}

fn ci_list_branches(ci: &Ci, user: &Userrc, out: &mut String) {
    writeln!(out, "<div> <table class=\"table\">").unwrap();

    for (b, _) in &user.branch {
        writeln!(out, "<tr> <th> <a href={}?branch={}>{}</a> </th> </tr>", ci.script_name, b, b).unwrap();
    }

    writeln!(out, "</table> </div>").unwrap();
}

fn ci_user(ci: &Ci) -> cgi::Response {
    let username = ci.user.as_ref().unwrap();
    let u = ci.rc.users.get(username);

    if u.is_none() {
        return error_response(format!("User {} not found", &username));
    }
    let u = u.unwrap();

    let mut out = String::new();

    writeln!(&mut out, "<!DOCTYPE HTML>").unwrap();
    writeln!(&mut out, "<html><head><title>CI branch list</title></head>").unwrap();
    writeln!(&mut out, "<link href=\"{}\" rel=\"stylesheet\">", ci.stylesheet).unwrap();

    writeln!(&mut out, "<body>").unwrap();

    match u {
        Ok(u)   => ci_list_branches(ci, &u, &mut out),
        Err(e)  => writeln!(out, "error parsing user config: {}", e).unwrap(),
    }

    writeln!(&mut out, "</body>").unwrap();
    writeln!(&mut out, "</html>").unwrap();

    cgi::html_response(200, out)
}

fn ci_list_users(ci: &Ci, out: &mut String) {
    writeln!(out, "<div> <table class=\"table\">").unwrap();
    for (i, _) in &ci.rc.users {
        writeln!(out, "<tr> <th> <a href={}?user={}>{}</a> </th> </tr>", ci.script_name, i, i).unwrap();
    }
    writeln!(out, "</table> </div>").unwrap();
}

fn ci_worker_status(ci: &Ci, out: &mut String) -> Option<()>{
    use chrono::prelude::Utc;

    let workers = workers_get(&ci.rc.ktest).ok()?;

    writeln!(out, "<div> <table class=\"table\">").unwrap();

    writeln!(out, "<tr>").unwrap();
    writeln!(out, "<th> Host.workdir   </th>").unwrap();
    writeln!(out, "<th> Commit         </th>").unwrap();
    writeln!(out, "<th> Tests          </th>").unwrap();
    writeln!(out, "<th> Elapsed time   </th>").unwrap();
    writeln!(out, "</tr>").unwrap();

    let now = Utc::now();
    let tests_dir = ci.rc.ktest.ktest_dir.clone().into_os_string().into_string().unwrap() + "/tests/";

    for w in workers {
        let elapsed = (now - w.starttime).max(Duration::zero());
        let tests = w.tests.strip_prefix(&tests_dir).unwrap_or(&w.tests);

        writeln!(out, "<tr>").unwrap();
        writeln!(out, "<td> {}.{}           </td>", w.hostname, w.workdir).unwrap();
        writeln!(out, "<td> {}~{}           </td>", w.branch, w.age).unwrap();
        writeln!(out, "<td> {}              </td>", tests).unwrap();
        writeln!(out, "<td> {}:{:02}:{:02}  </td>",
            elapsed.num_hours(),
            elapsed.num_minutes() % 60,
            elapsed.num_seconds() % 60).unwrap();
        writeln!(out, "</tr>").unwrap();
    }

    writeln!(out, "</table> </div>").unwrap();

    Some(())
}

fn ci_home(ci: &Ci) -> cgi::Response {
    let mut out = String::new();

    writeln!(&mut out, "<!DOCTYPE HTML>").unwrap();
    writeln!(&mut out, "<html><head><title>CI branch list</title></head>").unwrap();
    writeln!(&mut out, "<link href=\"{}\" rel=\"stylesheet\">", ci.stylesheet).unwrap();

    writeln!(&mut out, "<body>").unwrap();

    ci_list_users(ci, &mut out);

    ci_worker_status(ci, &mut out);

    writeln!(&mut out, "</body>").unwrap();
    writeln!(&mut out, "</html>").unwrap();

    cgi::html_response(200, out)
}

fn cgi_header_get(request: &cgi::Request, name: &str) -> String {
    request.headers().get(name)
        .map(|x| x.to_str())
        .transpose().ok().flatten()
        .map(|x| x.to_string())
        .unwrap_or(String::new())
}

fn error_response(msg: String) -> cgi::Response {
    let mut out = String::new();
    writeln!(&mut out, "{}", msg).unwrap();
    let env: Vec<_> = std::env::vars().collect();
    writeln!(&mut out, "env: {:?}", env).unwrap();
    cgi::text_response(200, out)
}

cgi::cgi_main! {|request: cgi::Request| -> cgi::Response {
    let rc = ciconfig_read();
    if let Err(e) = rc {
        return error_response(format!("could not read config; {}", e));
    }
    let rc = rc.unwrap();

    if !rc.ktest.output_dir.exists() {
        return error_response(format!("required file missing: JOBSERVER_OUTPUT_DIR (got {:?})",
                                      rc.ktest.output_dir));
    }

    unsafe {
        git2::opts::set_verify_owner_validation(false)
            .expect("set_verify_owner_validation should never fail");
    }

    let repo = git2::Repository::open(&rc.ktest.linux_repo);
    if let Err(e) = repo {
        return error_response(format!("error opening repository {:?}: {}", rc.ktest.linux_repo, e));
    }
    let repo = repo.unwrap();

    let query = cgi_header_get(&request, "x-cgi-query-string");
    let query: std::collections::HashMap<_, _> =
        querystring::querify(&query).into_iter().collect();

    let tests_matching = query.get("test").unwrap_or(&"");

    let ci = Ci {
        rc:                 rc,
        repo:               repo,
        stylesheet:         String::from(STYLESHEET),
        script_name:        cgi_header_get(&request, "x-cgi-script-name"),

        user:               query.get("user").map(|x| x.to_string()),
        branch:             query.get("branch").map(|x| x.to_string()),
        commit:             query.get("commit").map(|x| x.to_string()),
        tests_matching:     Regex::new(tests_matching).unwrap_or(Regex::new("").unwrap()),
    };

    if ci.commit.is_some() {
        ci_commit(&ci)
    } else if ci.branch.is_some() {
        ci_log(&ci)
    } else if ci.user.is_some() {
        ci_user(&ci)
    } else {
        ci_home(&ci)
    }
} }
