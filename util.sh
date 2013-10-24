
checkdep()
{
	which $1 > /dev/null
	if [ $? -ne 0 ]; then
		echo "Installing $1:"
		sudo apt-get install genisoimage
	fi
}
