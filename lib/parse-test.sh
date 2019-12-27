
parse_test_deps()
{
    ktest_cpus="6"
    ktest_mem=""
    ktest_timeout=""
    ktest_kernel_append=()
    ktest_images=()
    ktest_scratch_devs=()
    ktest_make_install=()
    ktest_kernel_config_require=()

    local NEXT_SCRATCH_DEV="b"
    local TESTPROG=$1
    local BUILD_ON_HOST=""

    require-lib()
    {
	local req="$1"

	pushd "$(dirname "$req")"	> /dev/null
	. $(basename "$req")
	popd				> /dev/null
    }

    do-build-deb()
    {
	local path=$(readlink -e "$1")
	local name=$(basename $path)

	get_tmpdir

	make -C "$path"

	cp -drl $path $ktest_tmp
	pushd "$ktest_tmp/$name" > /dev/null

	# make -nc actually work:
	rm -f debian/*.debhelper.log

	debuild --no-lintian -b -i -I -us -uc -nc
	popd > /dev/null
    }

    # $1 is a source repository, which will be built (with make) and then turned
    # into a dpkg
    require-build-deb()
    {
	local req=$1

	if ! [[ -d $req ]]; then
	    echo "build-deb dependency $req not found"
	    exit 1
	fi

	checkdep debuild devscripts

	run_quiet "building $(basename $req)" do-build-deb $req
    }

    require-make()
    {
	local req=$(readlink -e "$1")

	ktest_make_install+=("$req")

	if [[ -n $BUILD_ON_HOST ]]; then
	    run_quiet "building $1" make -C "$req"
	fi
    }

    require-kernel-config()
    {
	local OLDIFS=$IFS
	IFS=','

	for i in $1; do
	    ktest_kernel_config_require+=("$i")
	done

	IFS=$OLDIFS
    }

    require-kernel-append()
    {
	ktest_kernel_append+=($1)
    }

    config-scratch-devs()
    {
	ktest_scratch_devs+=("$1")
    }

    config-pmem-devs()
    {
	ktest_pmem_devs+=("$1")
    }

    config-image()
    {
	ktest_images+=("$1")
    }

    config-cpus()
    {
	ktest_cpus=$1
    }

    config-mem()
    {
	ktest_mem=$1
    }

    config-timeout()
    {
	n=$1
	if [ "${EXTENDED_DEBUG:-0}" == 1 ]; then
	    n=$((n * 2))
	fi
	ktest_timeout=$n
    }

    pushd "$(dirname "$TESTPROG")"	> /dev/null
    . $(basename "$TESTPROG")
    popd				> /dev/null

    if [ -z "$ktest_mem" ]; then
	echo "test must specify config-mem"
	exit 1
    fi

    if [ -z "$ktest_timeout" ]; then
	echo "test must specify config-timeout"
	exit 1
    fi
}
