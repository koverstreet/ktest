#!/bin/bash

set -o nounset
set -o errexit
set -o errtrace

[[ -f ~/.ktestrc ]] && . ~/.ktestrc

cd /home/bcachefs/linux

BRANCH=$1
COMMIT=$2
OUTPUT=$JOBSERVER_OUTPUT_DIR/c/$COMMIT
COMMITTEXT=$(git log -n1 $COMMIT)

echo "Generating summary for branch $BRANCH commit $COMMIT"

set +e
STATUSES=$(find "$OUTPUT" -name status)
grep -c PASSED			    $STATUSES	> $OUTPUT/nr_passed
grep -c FAILED			    $STATUSES	> $OUTPUT/nr_failed
grep -c NOTRUN			    $STATUSES	> $OUTPUT/nr_notrun
grep -c "NOT STARTED"		    $STATUSES	> $OUTPUT/nr_notstarted
grep -cvE '(PASSED|FAILED|NOTRUN)'  $STATUSES	> $OUTPUT/nr_unknown
echo $STATUSES|wc -w				> $OUTPUT/nr_tests
set -o errexit

echo "Running test2web"
#test2web "$COMMITTEXT" "$OUTPUT" > "$OUTPUT"/index.html

git_log_html()
{
    echo '<!DOCTYPE HTML>'
    echo "<html><head><title>$BRANCH</title></head>"
    echo '<link href="bootstrap.min.css" rel="stylesheet">'

    echo '<body>'
    echo '<div class="container">'
    echo '<table class="table">'

    echo "<tr>"
    echo "<th> Commit      </th>"
    echo "<th> Description </th>"
    echo "<th> Passed      </th>"
    echo "<th> Failed      </th>"
    echo "<th> Not started </th>"
    echo "<th> Not run     </th>"
    echo "<th> Unknown     </th>"
    echo "<th> Total       </th>"
    echo "</tr>"

    git log --pretty=oneline $BRANCH|
	while read LINE; do
	    COMMIT=$(echo $LINE|cut -d\  -f1)
	    COMMIT_SHORT=$(echo $LINE|cut -b1-14)
	    DESCRIPTION=$(echo $LINE|cut -d\  -f2-)
	    RESULTS=$JOBSERVER_OUTPUT_DIR/c/$COMMIT

	    [[ ! -d $RESULTS ]] && break

	    echo "<tr>"
	    echo "<td> <a href=\"c/$COMMIT\">$COMMIT_SHORT</a> </td>"
	    echo "<td> $DESCRIPTION </td>"
	    echo "<td> $(<$RESULTS/nr_passed)      </td>"
	    echo "<td> $(<$RESULTS/nr_failed)      </td>"
	    echo "<td> $(<$RESULTS/nr_notstarted)  </td>"
	    echo "<td> $(<$RESULTS/nr_notrun)      </td>"
	    echo "<td> $(<$RESULTS/nr_unknown)     </td>"
	    echo "<td> $(<$RESULTS/nr_tests)       </td>"
	    echo "</tr>"
	done

    echo "</table>"
    echo "</div>"
    echo "</body>"
    echo "</html>"
}

echo "Creating log for $BRANCH"
BRANCH_LOG=$(echo "$BRANCH"|tr / _).html
git_log_html > "$JOBSERVER_OUTPUT_DIR/$BRANCH_LOG"

echo "Running rsync"
flock --nonblock .rsync.lock rsync -r --delete $JOBSERVER_OUTPUT_DIR/ testdashboard@evilpiepirate.org:public_html || true

echo "Success"
