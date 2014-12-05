ROOTDIR := ../../..
SRC_ROOT := $(ROOTDIR)/src
BLD_ROOT := $(ROOTDIR)/bld
CXXFLAGS = -std=c++11 -O2 -Wall -Werror -D_FILE_OFFSET_BITS=64 -I.
CXXFLAGS += \
	   -g\
	   -I $(ROOTDIR)/usr/include	\
	   -I $(SRC_ROOT)/linux-bcache/drivers/dfs/client \
	   -I $(SRC_ROOT)/UnitTestCpp

LDFLAGS += -luuid
LDFLAGS1 += \
	    $(LDFLAGS) \
	   -L $(BLD_ROOT)/UnitTestCpp/.libs -Wl,-Bstatic -lUnitTest++ -Wl,-Bdynamic

default: all

all: dss_ioctl_test dss_span

SRCS= dss_ioctl_test.cpp dss_span.cpp
OBJS=$(SRCS:%.cpp=%.o)

-include $(SRCS:.cpp=.d)

%.o: %.cpp
	$(CXX) -c $(CXXFLAGS) $*.cpp -o $*.o
	$(CXX) $(CXXFLAGS) -MM -MF $*.d $*.cpp


dss_ioctl_test : dss_ioctl_test.o 
	 $(CXX) $(CXXFLAGS) -o $@ $< $(LDFLAGS1)

dss_span: dss_span.o 
	 $(CXX) $(CXXFLAGS) -o $@ $< $(LDFLAGS) 
