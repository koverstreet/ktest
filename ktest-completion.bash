# bash completion for ktest
#
#   . /path/to/ktest/ktest-completion.bash
#
# Test names complete relative to ktest's tests/ directory, matching what
# cmd_run() accepts: "fs/bcachefs/mount.ktest" from anywhere. The subtest
# after it completes too, from the test's own list-tests.

_ktest()
{
    local cur prev cmds ktest_dir tests_dir
    cur=${COMP_WORDS[COMP_CWORD]}
    prev=${COMP_WORDS[COMP_CWORD-1]}

    cmds="run boot ssh gdb kgdb mon sysrq screendump"

    if (( COMP_CWORD == 1 )); then
	COMPREPLY=( $(compgen -W "$cmds" -- "$cur") )
	return
    fi

    # Options take an argument we have nothing better to say about; let
    # readline fall back to filenames rather than offering a test name.
    case $prev in
	-a|-k|-o|-T|-n|-p)	COMPREPLY=(); compopt -o default; return;;
    esac

    [[ ${COMP_WORDS[1]} = run ]] || return

    ktest_dir=$(dirname "$(readlink -f "${COMP_WORDS[0]}")")
    tests_dir=$ktest_dir/tests
    [[ -d $tests_dir ]] || return

    # Which word is the test? The first one after "run" that isn't an option
    # or an option's argument. Everything after it is a subtest.
    local i test_word=0
    for (( i = 2; i < COMP_CWORD; i++ )); do
	case ${COMP_WORDS[i]} in
	    -a|-k|-o|-T|-n|-p)	(( i++ ));;
	    -*)			;;
	    *)			test_word=$i; break;;
	esac
    done

    if (( test_word )); then
	# Subtest names, from the test itself. list-tests sources the file,
	# which is cheap; a test that can't be listed simply offers nothing.
	local t
	if t=$(readlink -e "${COMP_WORDS[test_word]}") ||
	   t=$(readlink -e "$tests_dir/${COMP_WORDS[test_word]}"); then
	    COMPREPLY=( $(compgen -W "$("$t" list-tests 2>/dev/null)" -- "$cur") )
	fi
	return
    fi

    # A test name. Directories come back with a trailing slash so a second
    # tab descends rather than stopping on the directory itself.
    local f
    COMPREPLY=()
    for f in $(cd "$tests_dir" && compgen -f -- "$cur"); do
	if [[ -d $tests_dir/$f ]]; then
	    COMPREPLY+=( "$f/" )
	elif [[ $f = *.ktest ]]; then
	    COMPREPLY+=( "$f" )
	fi
    done

    compopt -o nospace
}

complete -F _ktest ktest ./ktest
