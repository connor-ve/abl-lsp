/* ttCustomer.i -- shared temp-table definitions */
&GLOBAL-DEFINE MAX-CUSTOMERS 500

DEFINE TEMP-TABLE ttCustomer NO-UNDO
    FIELD CustNum  AS INTEGER
    FIELD Name     AS CHARACTER FORMAT "x(30)"
    FIELD Balance  AS DECIMAL
    FIELD Region   AS CHARACTER
    INDEX idxCustNum IS PRIMARY UNIQUE CustNum.
