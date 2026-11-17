/* broken.p -- intentionally broken code to show diagnostics */
DEFINE VARIABLE cName AS CHARACTER NO-UNDO.
DEFINE VARIABLE cName AS CHARACTER NO-UNDO.   /* duplicate definition */
DEFINE VARIABLE iTotal NO-UNDO.               /* missing AS / LIKE */

{inc/doesNotExist.i}

PROCEDURE Compute:
    DO WHILE iTotal < 10:
        iTotal = (iTotal + 1.                 /* unclosed parenthesis */
END PROCEDURE.                                /* the DO block was never closed */

MESSAGE "this string never ends.
