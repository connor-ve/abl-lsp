/* format.p -- formats a report title */
DEFINE INPUT  PARAMETER pcTitle  AS CHARACTER NO-UNDO.
DEFINE OUTPUT PARAMETER pcResult AS CHARACTER NO-UNDO.

pcResult = CAPS(TRIM(pcTitle)).
