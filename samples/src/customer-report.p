/*------------------------------------------------------------------------
  customer-report.p
  Sample procedure exercising the ABL language server.
------------------------------------------------------------------------*/
BLOCK-LEVEL ON ERROR UNDO, THROW.

USING app.*.

{inc/ttCustomer.i}

DEFINE VARIABLE iCount   AS INTEGER   NO-UNDO.
DEFINE VARIABLE cTitle   AS CHARACTER NO-UNDO INITIAL "Customer report".
DEFINE VARIABLE oService AS OrderService NO-UNDO.
DEFINE BUFFER bCustomer FOR ttCustomer.

FUNCTION FormatBalance RETURNS CHARACTER (INPUT pdBalance AS DECIMAL) FORWARD.

oService = NEW OrderService().
RUN LoadCustomers.

FOR EACH ttCustomer NO-LOCK BY ttCustomer.Name:
    iCount = iCount + 1.
    DISPLAY ttCustomer.Name FormatBalance(ttCustomer.Balance) FORMAT "x(20)".
END.

IF iCount > {&MAX-CUSTOMERS} THEN
    MESSAGE "Too many customers" VIEW-AS ALERT-BOX WARNING.

oService:ProcessOrders(iCount).
RUN util/format.p (INPUT cTitle, OUTPUT cTitle).

PROCEDURE LoadCustomers:
    DEFINE VARIABLE i AS INTEGER NO-UNDO.

    DO i = 1 TO 3:
        CREATE bCustomer.
        ASSIGN
            bCustomer.CustNum = i
            bCustomer.Name    = SUBSTITUTE("Customer &1", i)
            bCustomer.Balance = i * 100.
    END.
END PROCEDURE.

FUNCTION FormatBalance RETURNS CHARACTER (INPUT pdBalance AS DECIMAL):
    RETURN STRING(pdBalance, "->>,>>9.99").
END FUNCTION.
