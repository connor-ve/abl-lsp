/* datasets.p -- datasets, data-relations, LIKE and BEFORE-TABLE */
DEFINE TEMP-TABLE ttCust NO-UNDO LIKE Customer
    FIELD Notes AS CHARACTER.
DEFINE TEMP-TABLE ttOrd NO-UNDO BEFORE-TABLE bttOrd
    FIELD OrderNum  AS INTEGER
    FIELD CustNum   LIKE Customer.CustNum
    FIELD OrderDate LIKE Order.OrderDate.

DEFINE VARIABLE cCity LIKE Customer.City NO-UNDO.

DEFINE DATASET dsCust FOR ttCust, ttOrd
    DATA-RELATION drOrders FOR ttCust, ttOrd RELATION-FIELDS (CustNum, CustNum).

FOR EACH ttCust:
    cCity = ttCust.City.
    FOR EACH ttOrd WHERE ttOrd.CustNum = ttCust.CustNum:
        DISPLAY ttOrd.OrderDate ttCust.Notes.
    END.
END.
FOR EACH bttOrd:
    DISPLAY bttOrd.OrderNum.
END.
DATASET dsCust:WRITE-JSON("FILE", "cust.json", TRUE).
