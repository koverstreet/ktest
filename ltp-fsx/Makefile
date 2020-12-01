PREFIX=/usr
INSTALL=install

ltp-fsx:
	cc -o ltp-fsx ltp-fsx.c
install: ltp-fsx
	$(INSTALL) -m0755 ltp-fsx $(DESTDIR)${PREFIX}/bin/
uninstall:
	rm -rf $(DESTDIR)${PREFIX}/bin/ltp-fsx
clean:
	rm -rf ltp-fsx
