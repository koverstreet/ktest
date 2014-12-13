/*
 * indent_stream.h
 *
 *  Created on: Dec 11, 2014
 *      Author: oleg
 *      from personal communications with mike spertus, mike_spertus@symantec.com
 *      under MIT license.
 *
 */

#ifndef INDENT_STREAM_H_
#define INDENT_STREAM_H_
#include <streambuf>
#include <iostream>

namespace rts {
class IndentStreamBuf : public std::streambuf
{
public:
    IndentStreamBuf(std::ostream &stream)
        : wrappedStream(stream), isLineStart(true), myIndent(0) {}
    virtual int overflow(int outputVal) override
    {
        if(outputVal == '\n') {
            isLineStart = true;
        } else if(isLineStart) {
            for(size_t i = 0; i < myIndent; i++) {
               wrappedStream << ' ';
            }
            isLineStart = false;
        }
        wrappedStream << static_cast<char>(outputVal);
        return outputVal;
    }
protected:
    std::ostream &wrappedStream;
    bool isLineStart;
public:
    size_t myIndent;
};

class IndentStream : public std::ostream
{
public:
    IndentStream(std::ostream &wrappedStream)
      : std::ostream(new IndentStreamBuf(wrappedStream)) {
    }
    ~IndentStream() { delete this->rdbuf(); }
protected:
    IndentStreamBuf *indentStreambuf;
};


std::ostream &indent(std::ostream &ostr);

std::ostream &unindent(std::ostream &ostr);
}
#endif /* INDENT_STREAM_H_ */
