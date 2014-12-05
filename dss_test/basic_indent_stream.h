/*
 * basic_indent_stream.h
 *
 *  Created on: Dec 11, 2014
 *      Author: oleg
 *      from personal communications with mike spertus, mike_spertus@symantec.com
 *      under MIT license.
 */
#ifndef BASIC_INDENT_STREAM_H
#  define BASIC_INDENT_STREAM_H

#include <typeinfo>
#include <ostream>
#include <streambuf>

// Avoid using entire std namespace to minimize leakage while still maintaining readability.
// This is the best that can be done at present. See http://www.open-std.org/jtc1/sc22/wg21/docs/papers/2007/n2386.pdf.
using std::char_traits;
using std::basic_streambuf;
using std::basic_ostream;
using std::streamsize;
using std::basic_string;
using std::bad_cast;
namespace rts {

template <class charT, class traits = char_traits<charT> >
class basic_indent_streambuf : public basic_streambuf<charT, traits>
{
public:
    typedef traits traits_type;
    typedef charT char_type;
    typedef typename traits_type::int_type int_type;

    basic_indent_streambuf
      (basic_streambuf<charT, traits> &_streambuf, charT spaceChar, charT nlChar)
        : myStreambuf(_streambuf),
          isLineStart(true),
          space(spaceChar),
          nl(nlChar),
          myIndent(0) {
    }

    virtual int_type overflow(int_type outputVal = traits_type::eof())
    {
        if(traits_type::eq_int_type(outputVal, traits_type::eof())) {
            return traits_type::eof();
        }
        char_type outputChar = traits_type::to_char_type(outputVal); // Safe because we excluded eof
        if(traits_type::eq(outputChar, nl)) {
            isLineStart = true;
        } else if(isLineStart) {
            myStreambuf.sputn(spaces.c_str(), myIndent);
            isLineStart = false;
        }
        return myStreambuf.sputc(outputChar);
    }

    static const int indentSpaces = 4;
    void increaseIndent() {
        myIndent += indentSpaces;
        while (myIndent > spaces.size()) {
            spaces += space;
        }
    }

    void decreaseIndent() {
        myIndent -= indentSpaces;
    }

protected:
    basic_streambuf<charT, traits> &myStreambuf;
    bool isLineStart;
    basic_string<charT> spaces;
    char_type space;
    char_type nl;
    streamsize myIndent;
};

template <class charT, class traits = std::char_traits<charT> >
class basic_indent_ostream : public basic_ostream<charT, traits>
{
public:
    basic_indent_ostream(basic_ostream<charT, traits> &wrappedStream)
      : basic_ostream<charT, traits> (0) {
        basic_ostream<charT, traits>::rdbuf(new basic_indent_streambuf<charT, traits>
                (*wrappedStream.rdbuf(),
                 this->widen(' '),
                 this->widen('\n')));
    }
    ~basic_indent_ostream() { delete this->rdbuf(); }
};

typedef basic_indent_ostream<char> indent_ostream;
typedef basic_indent_ostream<wchar_t> indent_wostream;


template <class charT, class traits>
basic_ostream<charT, traits> &indent(basic_ostream<charT, traits> &ostr)
{
    basic_indent_streambuf<charT, traits> *buf
      = dynamic_cast<basic_indent_streambuf<charT, traits> *>(ostr.rdbuf());
    if(buf) // Only honor for indent-aware streams
        buf->increaseIndent();
    return ostr;
}

template <class charT, class traits>
basic_ostream<charT, traits> &unindent(basic_ostream<charT, traits> &ostr)
{
    basic_indent_streambuf<charT, traits> *buf
      = dynamic_cast<basic_indent_streambuf<charT, traits> *>(ostr.rdbuf());
    if(buf) // Only honor for indent-aware streams
        buf->decreaseIndent();
    return ostr;
}
}
#endif

