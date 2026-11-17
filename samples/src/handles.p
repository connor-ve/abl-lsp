/* handles.p -- built-in functions, system handles and dynamic handles */
DEFINE VARIABLE hBuf   AS HANDLE    NO-UNDO.
DEFINE VARIABLE hQuery AS HANDLE    NO-UNDO.
DEFINE VARIABLE hFld   AS HANDLE    NO-UNDO.
DEFINE VARIABLE cName  AS CHARACTER NO-UNDO.

{inc/ttCustomer.i}

CREATE QUERY hQuery.
hBuf = BUFFER ttCustomer:HANDLE.
hQuery:SET-BUFFERS(hBuf).
hQuery:QUERY-PREPARE("FOR EACH ttCustomer").
hQuery:QUERY-OPEN().
hQuery:GET-FIRST().
hFld = hBuf:BUFFER-FIELD("Name").
cName = SUBSTRING(hFld:BUFFER-VALUE, 1, 10).
MESSAGE ENTRY(1, cName) SESSION:DATE-FORMAT TODAY.
cName = hQuery:GET-BUFFER-HANDLE(1):BUFFER-FIELD(1):NAME.
