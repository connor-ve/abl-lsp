/* locks.p -- for-each-without-lock, find-without-lock, unused-buffer, keyword-abbreviation */
DEFINE TEMP-TABLE ttWork NO-UNDO
    FIELD Id AS INTEGER.
DEFINE BUFFER bWork   FOR ttWork.
DEFINE BUFFER bCust   FOR Customer.
DEFINE BUFFER bUnused FOR Customer.
DEFINE VARIABLE iTotal AS INTEGER NO-UNDO.

FOR EACH Customer WHERE Customer.Balance > 0,
    EACH Order OF Customer NO-LOCK:
    iTotal = iTotal + 1.
END.

/* Temp-tables have no locks. */
FOR EACH ttWork:
    FIND FIRST bWork WHERE bWork.Id = ttWork.Id NO-ERROR.
END.

FIND FIRST bCust WHERE bCust.CustNum = 1 NO-ERROR.
FIND FIRST Customer EXCLUSIVE-LOCK NO-ERROR.

DISP iTotal.
MESSAGE INT("3") LOG(10).
