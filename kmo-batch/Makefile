
CXXFLAGS+=-O2 -Wall -Werror -g -std=c++11
CPPFLAGS := -D_FILE_OFFSET_BITS=64

all: kmo-batch

CXX_OBJS := $(patsubst %.cpp,%.o,$(wildcard *.cpp))
CXX_BINS := kmo-batch

kmo-batch: kmo-batch.o

$(CXX_BINS): %:
	$(CXX) $(LDFLAGS) $^ $(LDLIBS) -o $@

.PHONY: clean
clean:
	$(RM) $(TARGETS) $(OBJS) $(CXX_OBJS) $(DEP_FILES)
