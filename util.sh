
checkdep()
{
	COMMAND=$1
	PACKAGE=$2
	[ -z "$PACKAGE" ] && PACKAGE=$COMMAND

	if ! which $COMMAND > /dev/null; then
		echo -n "$COMMAND not found"

		if which apt-get > /dev/null; then
			echo ", installing $PACKAGE:"
			sudo apt-get install -y $PACKAGE
		else
			echo ", please install"
			exit 1
		fi
	fi
}
