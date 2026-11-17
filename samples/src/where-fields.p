/* where-fields.p -- unqualified field names resolve to the record phrase's table */
FOR EACH Customer NO-LOCK WHERE Balance > 100,
    EACH Order NO-LOCK WHERE Order.CustNum = Customer.CustNum AND OrderDate <> ?:
    DISPLAY Name OrderDate.
END.

FOR EACH Order NO-LOCK WHERE CustNum = 1:
    DISPLAY OrderNum.
END.

FIND FIRST Order NO-LOCK WHERE CustNum = 2 NO-ERROR.
MESSAGE CAN-FIND(FIRST Customer WHERE CustNum = 3).
