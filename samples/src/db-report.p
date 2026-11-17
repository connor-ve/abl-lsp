/* db-report.p -- uses the database schema in db/sports.df */
DEFINE BUFFER bCust FOR Customer.

FOR EACH Customer NO-LOCK WHERE Customer.Balance > 1000:
    DISPLAY Customer.Name sports.Customer.City.
END.

FIND FIRST bCust NO-LOCK WHERE bCust.CustNum = 1 NO-ERROR.
IF AVAILABLE bCust THEN
    MESSAGE bCust.Name.

FOR EACH sports.Order NO-LOCK:
    DISPLAY Order.OrderDate.
END.
