/*
 * indent_stream.cpp
 *
 *  Created on: Dec 11, 2014
 *      Author: oleg
 *      from personal communications with mike spertus, mike_spertus@symantec.com
 *      under MIT license.
 *
 */
#include "indent_stream.h"

namespace rts {
std::ostream &indent(std::ostream &ostr)
{
    IndentStreamBuf *out = dynamic_cast<IndentStreamBuf *>(ostr.rdbuf());
    if(0 != out) {
        out->myIndent += 4;
    }
    return ostr;
}

std::ostream &unindent(std::ostream &ostr)
{
    IndentStreamBuf *out = dynamic_cast<IndentStreamBuf *>(ostr.rdbuf());
    out->myIndent -= 4;
    return ostr;
}
}



