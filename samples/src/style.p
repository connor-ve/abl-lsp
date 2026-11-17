/* style.p -- missing-no-undo and find-without-no-error */
DEFINE VARIABLE cUndoable AS CHARACTER INITIAL "x".
DEFINE VARIABLE cFine     AS CHARACTER NO-UNDO.
def var iShort as int. /* lint-ignore keyword-abbreviation */
DEFINE TEMP-TABLE ttWork
    FIELD Id AS INTEGER.
DEFINE TEMP-TABLE ttFine NO-UNDO
    FIELD Id AS INTEGER.

FIND FIRST ttWork WHERE ttWork.Id = 1.
FIND FIRST ttFine WHERE ttFine.Id = 1 NO-ERROR.
FIND CURRENT ttWork.
FIND LAST ttWork. /* lint-ignore find-without-no-error */

MESSAGE cUndoable cFine iShort.
