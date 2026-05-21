use regex::Regex;
use std::fmt::Write;
extern crate cgi;
extern crate querystring;

use ci_cgi::{
    branch_get_results, ciconfig_read, last_good_line, update_lcov, CiConfig,
    CommitResults, TestResultsMap, TestStatus, Userrc,
};

const STYLESHEET: &str = "bootstrap.min.css";

const COMMIT_CSS_JS:    &str =
"
<style>
        .toplevel-container {
                margin-left: 10px;
                height: 100%;
        }
        .header {
                position: absolute;
        }
        .horizontal-container {
                display: flex;
                flex-direction: row;
        }
        .horizontal {
                margin-right: 10px;
                overflow-y: scroll
        }
        .filtered {
                display: none;
        }
        #filters {
                margin: 1em 0;
        }
        #filters label {
                margin-left: 0.3em;
        }
        .table.no-wrap,
        .table.no-wrap td,
        .table.no-wrap th {
                white-space: nowrap;
        }
</style>
<script>
        document.getElementById('myLink').addEventListener('click', function(event) {
                event.preventDefault();
                alert('Link clicked!');
        });
        function getSelectedRadioValue(name) {                
                const radios = document.getElementsByName(name);
                                                                
                for (let i = 0; i < radios.length; i++) {       
                        if (radios[i].checked) {
                                return radios[i].value;            
                        }                                                 
                }                                                         
                                                                          
                return null;                                   
        }                                                   
                                                            
        function get_row_status(el) {
                return el.querySelector('td:nth-child(3)').textContent.trim()                                                
        }                                                                                                                    
                                                                                                                             
        function update_filter() {                                                                                           
                const v = getSelectedRadioValue('testfilter');                                                               
                const el_table = document.querySelector('table')                                                             
                                                                                                                             
                for (const el of el_table.querySelectorAll('tr')) {                                                          
                        if (!v || v == 'All' || v == get_row_status(el)) {                                                   
                                el.classList.remove('filtered')                                                              
                        } else {                                                                                             
                                el.classList.add('filtered')                                                                 
                        }                                                                                                    
                }                                                                                                            
        }
</script>
";

const COMMIT_FILTER:    &str =
"
<div id=filters>
    Filter by:                                         
    <label><input type=radio name=testfilter onchange='update_filter()' value='Passed'>         Passed</label>
    <label><input type=radio name=testfilter onchange='update_filter()' value='Failed' checked> Failed</label>
    <label><input type=radio name=testfilter onchange='update_filter()' value='Not run'>        Not run</label>
    <label><input type=radio name=testfilter onchange='update_filter()' value='Not started'>    Not started</label>
    <label><input type=radio name=testfilter onchange='update_filter()' value='In progress'>    In progress</label>
    <label><input type=radio name=testfilter onchange='update_filter()' value='Unknown'>        Unknown</label>
    <label><input type=radio name=testfilter onchange='update_filter()' value='All'>            All</label>
</div>
";

struct Ci {
    rc: CiConfig,
    repo: git2::Repository,
    stylesheet: String,
    script_name: String,

    user: Option<String>,
    branch: Option<String>,
    commit: Option<String>,
    tests_matching: Regex,
}

fn ci_branch_get_results(ci: &Ci) -> Result<Vec<CommitResults>, String> {
    branch_get_results(
        &ci.repo,
        &ci.rc.ktest,
        ci.user.as_deref(),
        ci.branch.as_deref(),
        ci.commit.as_deref(),
        &ci.tests_matching,
    )
}

fn ci_log(ci: &Ci) -> cgi::Response {
    let mut out = String::new();
    let branch = ci.branch.as_ref().unwrap();

    let commits = ci_branch_get_results(ci);
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
    writeln!(
        &mut out,
        "<link href=\"{}\" rel=\"stylesheet\">",
        ci.stylesheet
    )
    .unwrap();

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
                    writeln!(
                        &mut out,
                        "<tr> <td> ({} untested commits) </td> </tr>",
                        nr_empty
                    )
                    .unwrap();
                    nr_empty = 0;
                }

                fn count(r: &TestResultsMap, t: TestStatus) -> usize {
                    r.iter().filter(|x| x.1.status == t).count()
                }

                let subject_len = r.message.find('\n').unwrap_or(r.message.len());

                let duration: u64 = r.tests.iter().map(|x| x.1.duration).sum();

                writeln!(&mut out, "<tr>").unwrap();
                writeln!(
                    &mut out,
                    "<td> <a href=\"{}?user={}&branch={}&commit={}\">{}</a> </td>",
                    ci.script_name,
                    ci.user.as_ref().unwrap(),
                    branch,
                    r.id,
                    &r.id.as_str()[..14]
                )
                .unwrap();
                writeln!(&mut out, "<td> {} </td>", &r.message[..subject_len]).unwrap();
                writeln!(
                    &mut out,
                    "<td> {} </td>",
                    count(&r.tests, TestStatus::Passed)
                )
                .unwrap();
                writeln!(
                    &mut out,
                    "<td> {} </td>",
                    count(&r.tests, TestStatus::Failed)
                )
                .unwrap();
                writeln!(
                    &mut out,
                    "<td> {} </td>",
                    count(&r.tests, TestStatus::Notstarted)
                )
                .unwrap();
                writeln!(
                    &mut out,
                    "<td> {} </td>",
                    count(&r.tests, TestStatus::Notrun)
                )
                .unwrap();
                writeln!(
                    &mut out,
                    "<td> {} </td>",
                    count(&r.tests, TestStatus::Inprogress)
                )
                .unwrap();
                writeln!(
                    &mut out,
                    "<td> {} </td>",
                    count(&r.tests, TestStatus::Unknown)
                )
                .unwrap();
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
                    writeln!(
                        &mut out,
                        "<tr> <td> ({} untested commits) </td> </tr>",
                        nr_empty
                    )
                    .unwrap();
                    nr_empty = 0;
                }

                let subject_len = r.message.find('\n').unwrap_or(r.message.len());

                writeln!(&mut out, "<tr class={}>", t.1.status.table_class()).unwrap();
                writeln!(
                    &mut out,
                    "<td> <a href=\"{}?branch={}&commit={}\">{}</a> </td>",
                    ci.script_name,
                    branch,
                    r.id,
                    &r.id.as_str()[..14]
                )
                .unwrap();
                writeln!(&mut out, "<td> {} </td>", &r.message[..subject_len]).unwrap();
                writeln!(&mut out, "<td> {} </td>", t.1.status.to_str()).unwrap();
                writeln!(&mut out, "<td> {}s </td>", t.1.duration).unwrap();
                writeln!(
                    &mut out,
                    "<td> <a href=c/{}/{}/log.br>        log                 </a> </td>",
                    &r.id, t.0
                )
                .unwrap();
                writeln!(
                    &mut out,
                    "<td> <a href=c/{}/{}/full_log.br>   full log            </a> </td>",
                    &r.id, t.0
                )
                .unwrap();
                writeln!(
                    &mut out,
                    "<td> <a href=c/{}/{}>		        output directory    </a> </td>",
                    &r.id, t.0
                )
                .unwrap();
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

fn log_link(out: &mut String, fname: &str, link: &str) {
    let onclick = format!(
        "fetch('{}')
    .then(response => response.text())
    .then(text => {{
        document.getElementById('testlog').textContent = text;
  }});
  return false;
",
        &fname
    );

    writeln!(
        out,
        "<td> <a href={} onclick=\"{}\"> {} </a> </td>",
        fname, onclick, link
    )
    .unwrap();
}

fn ci_commit(ci: &Ci) -> cgi::Response {
    let mut out = String::new();

    let commits = ci_branch_get_results(ci);
    if let Err(e) = commits {
        return error_response(e);
    }
    let commits = commits.unwrap();

    let first_commit = &commits[0];
    let message = &first_commit.message;
    let subject_len = message.find('\n').unwrap_or(message.len());

    update_lcov(&ci.rc.ktest, &first_commit.id);

    writeln!(&mut out, "<!DOCTYPE HTML>").unwrap();

    out.push_str(COMMIT_CSS_JS);

    writeln!(&mut out, "<div class=\"header\">").unwrap();
    writeln!(
        &mut out,
        "<html><head><title>{}</title></head>",
        &message[..subject_len]
    )
    .unwrap();
    writeln!(
        &mut out,
        "<link href=\"{}\" rel=\"stylesheet\">",
        ci.stylesheet
    )
    .unwrap();

    writeln!(&mut out, "<body>").unwrap();
    writeln!(&mut out, "<div class=\"toplevel-container\">").unwrap();

    writeln!(&mut out, "<h3><th>{}</th></h3>", &message[..subject_len]).unwrap();

    if ci
        .rc
        .ktest
        .output_dir
        .join(&first_commit.id)
        .join("lcov")
        .exists()
    {
        writeln!(
            &mut out,
            "<p> <a href=c/{}/lcov> Code coverage </a> </p>",
            &first_commit.id
        )
        .unwrap();
    }

    out.push_str(COMMIT_FILTER);
    writeln!(&mut out, "</div>").unwrap();

    writeln!(&mut out, "<div class=\"horizontal-container\">").unwrap();

    writeln!(&mut out, "<div class=\"horizontal\">").unwrap();
    writeln!(&mut out, "<table class=\"table no-wrap\">").unwrap();
    for (name, result) in &first_commit.tests {
        writeln!(&mut out, "<tr class={}>", result.status.table_class()).unwrap();
        log_link(
            &mut out,
            &format!("c/{}/{}/log.br", &first_commit.id, name),
            name,
        );
        writeln!(&mut out, "<td> {}s </td>", result.duration).unwrap();
        writeln!(&mut out, "<td> {}  </td>", result.status.to_str()).unwrap();
        writeln!(&mut out, "<td> {}  </td>", last_good_line(&commits, name)).unwrap();
        if let Some(branch) = &ci.branch {
            writeln!(
                &mut out,
                "<td> <a href={}?user={}&branch={}&test=^{}$> git log        </a> </td>",
                ci.script_name,
                ci.user.as_ref().unwrap(),
                &branch,
                name
            )
            .unwrap();
        }

        log_link(
            &mut out,
            &format!("c/{}/{}/full_log.br", &first_commit.id, name),
            "full",
        );

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

    writeln!(&mut out, "<script>update_filter()</script>").unwrap();

    writeln!(&mut out, "</div>").unwrap();
    writeln!(&mut out, "</body>").unwrap();
    writeln!(&mut out, "</html>").unwrap();
    cgi::html_response(200, out)
}

fn ci_list_branches(ci: &Ci, user: &Userrc, out: &mut String) {
    writeln!(out, "<div> <table class=\"table\">").unwrap();

    for (b, _) in &user.branches {
        writeln!(
            out,
            "<tr> <th> <a href={}?user={}&branch={}>{}</a> </th> </tr>",
            ci.script_name,
            ci.user.as_ref().unwrap(),
            b,
            b
        )
        .unwrap();
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
    writeln!(
        &mut out,
        "<link href=\"{}\" rel=\"stylesheet\">",
        ci.stylesheet
    )
    .unwrap();

    writeln!(&mut out, "<body>").unwrap();

    match u {
        Ok(u) => ci_list_branches(ci, &u, &mut out),
        Err(e) => writeln!(out, "error parsing user config: {}", e).unwrap(),
    }

    writeln!(&mut out, "</body>").unwrap();
    writeln!(&mut out, "</html>").unwrap();

    cgi::html_response(200, out)
}

fn ci_list_users(ci: &Ci, out: &mut String) {
    writeln!(out, "<h4>Users</h4>").unwrap();
    writeln!(out, "<div> <table class=\"table\">").unwrap();
    writeln!(
        out,
        "<tr> <th> User </th> <th> Nice </th> <th> Branches </th> </tr>"
    )
    .unwrap();

    for (user, userrc) in &ci.rc.users {
        let nice = ci.rc.ktest.user_nice.get(user).copied().unwrap_or(0);
        let branches: String = userrc
            .as_ref()
            .map(|u| u.branches.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|_| "error".to_string());

        writeln!(out, "<tr>").unwrap();
        writeln!(
            out,
            "<td> <a href=\"{}?user={}\">{}</a> </td>",
            ci.script_name, user, user
        )
        .unwrap();
        writeln!(out, "<td> {} </td>", nice).unwrap();
        writeln!(out, "<td> {} </td>", branches).unwrap();
        writeln!(out, "</tr>").unwrap();
    }

    writeln!(out, "</table> </div>").unwrap();
}

fn ci_home(ci: &Ci) -> cgi::Response {
    let mut out = String::new();

    writeln!(&mut out, "<!DOCTYPE HTML>").unwrap();
    writeln!(&mut out, "<html><head><title>CI branch list</title></head>").unwrap();
    writeln!(
        &mut out,
        "<link href=\"{}\" rel=\"stylesheet\">",
        ci.stylesheet
    )
    .unwrap();

    writeln!(&mut out, "<body>").unwrap();

    writeln!(
        &mut out,
        "<p><a href=\"{}?status\">→ live CI status</a></p>",
        ci.script_name
    )
    .unwrap();

    ci_list_users(ci, &mut out);

    writeln!(&mut out, "</body>").unwrap();
    writeln!(&mut out, "</html>").unwrap();

    cgi::html_response(200, out)
}

fn cgi_header_get(request: &cgi::Request, name: &str) -> String {
    request
        .headers()
        .get(name)
        .map(|x| x.to_str())
        .transpose()
        .ok()
        .flatten()
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


/// CSS for the live status page.
const STATUS_CSS: &str = "
body { margin: 1em; }
.exec { display: inline-block; vertical-align: top; margin: 0 1em 1em 0;
        min-width: 15em; border: 1px solid #ccc; padding: .4em .6em; }
.exec h5 { margin: 0 0 .3em; }
.job { cursor: pointer; padding: 0 3px; }
.job:hover { background: #eef; }
td.running, .running { color: #060; }
td.pending { color: #888; }
td.failed, .failed { color: #c00; }
td.cancelled { color: #c60; }
#logview { background: #111; color: #ddd; padding: .5em; height: 26em;
           overflow-y: auto; white-space: pre-wrap; font: 12px/1.3 monospace; }
";

/// Body of the live status page; the script fills the divs.
const STATUS_BODY: &str = "
<div class=container-fluid>
<h3>CI status</h3>
<div id=summary></div>
<div id=fairshare></div>
<h4>Executors</h4>
<div id=executors></div>
<details>
<summary>Jobs</summary>
<div id=jobs></div>
</details>
<h4>Log <span id=logname></span></h4>
<pre id=logview>click a job to tail its log</pre>
</div>
";

/// The live status app: polls the daemon's status JSON every 2s,
/// renders the executor grid + job table, and tails a job's log on
/// click (via Range requests against the web-served log file).
const STATUS_JS: &str = r#"
const STATUS_URL = 'c/ci-daemon-status.json';
let curLog = null, logTimer = null;

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

// JobInfo.log_path is an absolute server path; the logs are web-served
// under the output dir at .../ci-daemon-logs/...
function logUrl(p) {
  if (!p) return null;
  const i = p.indexOf('ci-daemon-logs/');
  return i < 0 ? null : 'c/' + p.slice(i);
}

function fmtDur(s) {
  s = Math.floor(s);
  const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60;
  return h ? h + 'h' + m + 'm' : (m ? m + 'm' + (s % 60) + 's' : s + 's');
}

function tailLog() {
  if (!curLog) return;
  fetch(curLog.url, { headers: { Range: 'bytes=' + curLog.offset + '-' } })
    .then(r => r.status === 416 ? '' : r.text())
    .then(t => {
      if (!t) return;
      const v = document.getElementById('logview');
      const atBottom = v.scrollTop + v.clientHeight >= v.scrollHeight - 4;
      v.textContent += t;
      curLog.offset += new Blob([t]).size;
      if (atBottom) v.scrollTop = v.scrollHeight;
    })
    .catch(() => {});
}

function openLog(name, url) {
  curLog = { url: url, offset: 0 };
  document.getElementById('logname').textContent = '- ' + name;
  document.getElementById('logview').textContent = '';
  if (logTimer) clearInterval(logTimer);
  logTimer = setInterval(tailLog, 2000);
  tailLog();
}

function render(s) {
  const byId = {};
  for (const j of s.jobs) byId[j.id] = j;

  // running/finished are counted from s.jobs (non-pending, all
  // carried); the pending backlog is summarized per group.
  const counts = {};
  for (const j of s.jobs) counts[j.status] = (counts[j.status] || 0) + 1;
  const pend = Object.entries(s.pending_by_group || {}).sort((a, b) => b[1] - a[1]);
  const pendTotal = pend.reduce((acc, e) => acc + e[1], 0);
  const pendStr = pend.map(e => e[0] + ' ' + e[1]).join(', ');
  document.getElementById('summary').textContent =
    pendTotal + ' pending' + (pendStr ? ' (' + pendStr + ')' : '') +
    ' · ' + (counts.running || 0) + ' running' +
    ' · ' + (counts.completed || 0) + ' completed' +
    ' · ' + (counts.failed || 0) + ' failed';

  // Fair-share standing: groups (users) by decayed recent farm time,
  // lowest first — that is who the scheduler serves next.
  const groups = Object.entries(s.fairshare || {}).sort((a, b) => a[1] - b[1]);
  document.getElementById('fairshare').textContent = groups.length
    ? 'fair-share: ' + groups.map(g => g[0] + ' ' + fmtDur(g[1])).join('   ')
    : '';

  // Executors are per-slot; group them back by host. Every executor is
  // shown — idle ones included — each tailing its own log.
  const ex = document.getElementById('executors');
  ex.textContent = '';
  const hosts = {};
  for (const e of s.executors)
    (hosts[e.host] = hosts[e.host] || []).push(e);
  for (const host of Object.keys(hosts).sort()) {
    const execs = hosts[host].sort((a, b) => a.name.localeCompare(b.name));
    const running = execs.filter(e => e.current_jobs.length).length;
    const box = el('div', 'exec');
    box.appendChild(el('h5', null, host + ' (' + running + '/' + execs.length + ')'));
    for (const e of execs) {
      const j = e.current_jobs.length ? byId[e.current_jobs[0]] : null;
      const row = el('div', 'job' + (j ? ' running' : ''),
                     e.name + ' — ' + (j ? j.name : 'idle'));
      const u = logUrl(e.log_path);
      if (u) row.onclick = () => openLog(e.name, u);
      box.appendChild(row);
    }
    ex.appendChild(box);
  }

  const order = { running: 0, pending: 1, failed: 2, cancelled: 3, completed: 4 };
  const jobs = s.jobs.slice().sort((a, b) =>
    (order[a.status] - order[b.status]) || a.name.localeCompare(b.name));
  const tbl = el('table', 'table');
  const head = el('tr');
  for (const h of ['Status', 'User', 'Job', 'Elapsed', 'Log'])
    head.appendChild(el('th', null, h));
  tbl.appendChild(head);
  for (const j of jobs) {
    const tr = el('tr');
    tr.appendChild(el('td', j.status, j.status));
    tr.appendChild(el('td', null, j.group || '-'));
    tr.appendChild(el('td', null, j.name));
    tr.appendChild(el('td', null, fmtDur(j.elapsed_secs)));
    const td = el('td');
    const u = logUrl(j.log_path);
    if (u) {
      const a = el('span', 'job', 'log');
      a.onclick = () => openLog(j.name, u);
      td.appendChild(a);
    } else {
      td.textContent = '-';
    }
    tr.appendChild(td);
    tbl.appendChild(tr);
    if (j.error) {
      const er = el('tr'), etd = el('td', 'failed', j.error);
      etd.colSpan = 5;
      er.appendChild(etd);
      tbl.appendChild(er);
    }
  }
  const jdiv = document.getElementById('jobs');
  jdiv.textContent = '';
  jdiv.appendChild(tbl);
}

function poll() {
  fetch(STATUS_URL)
    .then(r => r.json())
    .then(render)
    .catch(() => {
      document.getElementById('summary').textContent = 'daemon status unavailable';
    });
}
poll();
"#;

/// The live push-mode status page — a small client-side app over the
/// status JSON the daemon writes every couple seconds. See STATUS_JS.
fn ci_status_page(ci: &Ci) -> cgi::Response {
    let page = format!(
        "<!DOCTYPE html>\n\
         <html><head><title>CI status</title>\n\
         <link href=\"{}\" rel=\"stylesheet\">\n\
         <style>{}</style></head>\n\
         <body>{}<script>{}</script></body></html>\n",
        ci.stylesheet, STATUS_CSS, STATUS_BODY, STATUS_JS,
    );
    cgi::html_response(200, page)
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

    let query_string = cgi_header_get(&request, "x-cgi-query-string");
    let query: std::collections::HashMap<_, _> =
        querystring::querify(&query_string).into_iter().collect();

    let tests_matching = query.get("test").unwrap_or(&"");

    /* The dashboard handles multiple repos (linux, bcachefs-tools, ...) —
     * pick the right one for the current (user, branch) request. Mirrors
     * gen-job-list's per-branch repo lookup. */
    let repo_path = (|| -> Option<&std::path::Path> {
        let user = query.get("user")?;
        let branch = query.get("branch")?;
        let userrc = rc.users.get(*user)?.as_ref().ok()?;
        let branchconfig = userrc.branches.get(*branch)?;
        rc.ktest.repo_path(&branchconfig.repo)
    })().unwrap_or(rc.ktest.linux_repo.as_path());

    let repo = git2::Repository::open(repo_path);
    if let Err(e) = repo {
        return error_response(format!("error opening repository {:?}: {}", repo_path, e));
    }
    let repo = repo.unwrap();

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

    // querify() drops a bare key with no '=', so check the raw string.
    if query_string.split('&').any(|p| p == "status" || p.starts_with("status=")) {
        ci_status_page(&ci)
    } else if ci.user.is_some() {
        if ci.commit.is_some() {
            ci_commit(&ci)
        } else if ci.branch.is_some() {
            ci_log(&ci)
        } else {
            ci_user(&ci)
        }
    } else {
        ci_home(&ci)
    }
} }
