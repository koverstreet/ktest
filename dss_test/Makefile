ROOTDIR := ../../..
CXXFLAGS += \
	   -I $(ROOTDIR)/usr/include	\
	   -I $(ROOTDIR)/src/linux-bcache/drivers/dfs/client 

dss_ioctl_test : dss_ioctl_test.o
	 $(CXX) $(CXXFLAGS) -o $@ $<
