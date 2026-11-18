/* Uses the abl-collection library, a package from git (see abl.toml and abl.lock).
   Run `abl i` in samples/packages first: it puts the library in vendor/.
   Then hover IntSlice or Map, F12 into the library, and try completion after oNums:. */
USING abl.Collections.IntSlice FROM PROPATH.
USING abl.Collections.Map FROM PROPATH.

DEFINE VARIABLE oNums  AS IntSlice NO-UNDO.
DEFINE VARIABLE oPrefs AS Map      NO-UNDO.

oNums = IntSlice:Of(5, 3, 9).
oNums:Append(1):Sort().
MESSAGE "sum" oNums:Sum() "max" oNums:Max() "first" oNums:First().

oPrefs = NEW Map().
oPrefs:Put("theme", "dark"):Put("pageSize", 25).
MESSAGE oPrefs:GetChar("theme", "light") oPrefs:GetInt("pageSize", 10).
