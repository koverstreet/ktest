
checkdep()
{
	COMMAND=$1

	if [[ $# -ge 2 ]]; then
	    PACKAGE=$2
	else
	    PACKAGE=$COMMAND
	fi

	if ! which "$COMMAND" > /dev/null; then
		echo -n "$COMMAND not found"

		if which apt-get > /dev/null && \
			which sudo > /dev/null; then
			echo ", installing $PACKAGE:"
			sudo apt-get install -y "$PACKAGE"
		else
			echo ", please install"
			exit 1
		fi
	fi
}
