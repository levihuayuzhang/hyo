# Generates fixtures/excel-golden.xlsx and fixtures/excel-golden.tsv from the
# locally installed Microsoft Excel. This is a development tool; the committed
# fixture lets CI verify compatibility without Excel.
#
# Run with an interactive desktop / COM access:
#   powershell.exe -Sta -ExecutionPolicy Bypass -File scripts\make-excel-golden.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$fixtures = Join-Path $root 'fixtures'
New-Item -ItemType Directory -Force $fixtures | Out-Null
$xlsx = Join-Path $fixtures 'excel-golden.xlsx'
$tsv = Join-Path $fixtures 'excel-golden.tsv'

$cases = @(
    @('Arithmetic precedence', '=1+2*3'),
    @('Unary minus vs power', '=-2^2'),
    @('Power right associative', '=2^3^2'),
    @('Division', '=10/4'),
    @('One third', '=1/3'),
    @('Point one plus point two', '=0.1+0.2'),
    @('Overflow', '=2^1024'),
    @('Division by zero', '=1/0'),
    @('Square root domain', '=SQRT(-1)'),
    @('Round trap', '=ROUND(2.675,2)'),
    @('Round negative trap', '=ROUND(-2.675,2)'),
    @('Round hundreds', '=ROUND(1234,-2)'),
    @('Round half away', '=ROUND(2.5,0)'),
    @('Mod negative', '=MOD(-3,2)'),
    @('Sum range', '=SUM(Inputs!A1:A5)'),
    @('Average range', '=AVERAGE(Inputs!A1:A5)'),
    @('Count range', '=COUNT(Inputs!A1:A5)'),
    @('Counta range', '=COUNTA(Inputs!A1:A5)'),
    @('Countblank range', '=COUNTBLANK(Inputs!A1:A5)'),
    @('Min range', '=MIN(Inputs!A1:A5)'),
    @('Max range', '=MAX(Inputs!A1:A5)'),
    @('Product range', '=PRODUCT(Inputs!A1:A5)'),
    @('Sumproduct', '=SUMPRODUCT(Inputs!B1:B3,Inputs!C1:C3)'),
    @('If lazy', '=IF(TRUE,1,1/0)'),
    @('Iferror', '=IFERROR(1/0,42)'),
    @('Ifna', '=IFNA(NA(),42)'),
    @('And', '=AND(TRUE,1,2)'),
    @('Or', '=OR(FALSE,0,1)'),
    @('Concat', '=CONCAT("a",1,TRUE)'),
    @('Left', '=LEFT("hello",2)'),
    @('Right', '=RIGHT("hello",2)'),
    @('Mid', '=MID("hello",2,3)'),
    @('Len text', '=LEN("hello")'),
    @('Upper', '=UPPER("abc")'),
    @('Trim', '=TRIM("  a   b  ")'),
    @('Text compare', '="a"="A"'),
    @('Number vs text', '=1="1"'),
    @('Less than', '=1<2'),
    @('Exact', '=EXACT("a","A")'),
    @('Log10', '=LOG10(1000)'),
    @('Log default base', '=LOG(100)'),
    @('Power', '=POWER(2,10)'),
    @('Abs', '=ABS(-5)'),
    @('Sign', '=SIGN(-5)'),
    @('Int', '=INT(-2.5)'),
    @('Empty compare', '=Inputs!A5=""')
)

$excel = $null
$workbook = $null
try {
    $excel = New-Object -ComObject Excel.Application
    $excel.Visible = $false
    $excel.DisplayAlerts = $false
    $excel.ScreenUpdating = $false

    $workbook = $excel.Workbooks.Add()
    $inputs = $workbook.Worksheets.Item(1)
    $inputs.Name = 'Inputs'
    $casesSheet = $workbook.Worksheets.Add()
    $casesSheet.Name = 'Cases'
    $casesSheet.Move($inputs)

    $inputs.Range('A1').Value2 = 10
    $inputs.Range('A2').Value2 = 20
    $inputs.Range('A3').Value2 = 'text'
    $inputs.Range('A4').Value2 = $true
    $inputs.Range('A5').ClearContents() | Out-Null
    $inputs.Range('B1').Value2 = 1
    $inputs.Range('B2').Value2 = 2
    $inputs.Range('B3').Value2 = 3
    $inputs.Range('C1').Value2 = 4
    $inputs.Range('C2').Value2 = 5
    $inputs.Range('C3').Value2 = 6

    $casesSheet.Range('A1').Value2 = 'Case'
    $casesSheet.Range('B1').Value2 = 'Formula'
    $casesSheet.Range('C1').Value2 = 'Excel result'
    $casesSheet.Range('A1:C1').Font.Bold = $true
    $casesSheet.Columns.Item(1).ColumnWidth = 28
    $casesSheet.Columns.Item(2).ColumnWidth = 42
    $casesSheet.Columns.Item(3).ColumnWidth = 34

    $row = 2
    foreach ($case in $cases) {
        $name = $case[0]
        $formula = $case[1]
        $casesSheet.Cells.Item($row, 1).Value2 = $name
        $source = $casesSheet.Cells.Item($row, 2)
        $source.NumberFormat = '@'
        $source.Value2 = $formula
        $casesSheet.Cells.Item($row, 3).Formula = $formula
        $row++
    }

    $excel.CalculateFullRebuild()
    $workbook.SaveAs($xlsx, 51) # xlOpenXMLWorkbook
    $excel.CalculateFullRebuild()

    $lines = New-Object System.Collections.Generic.List[string]
    $lines.Add("case`tsheet`taddress`tformula`ttext`ttype`tvalue")
    for ($r = 2; $r -lt $row; $r++) {
        $name = [string]$casesSheet.Cells.Item($r, 1).Value2
        $formula = [string]$casesSheet.Cells.Item($r, 2).Value2
        $cell = $casesSheet.Cells.Item($r, 3)
        $text = [string]$cell.Text
        $value2 = $cell.Value2
        $type = if ($null -eq $value2) { 'Empty' } else { $value2.GetType().Name }
        $raw = if ($null -eq $value2) {
            ''
        } elseif ($value2 -is [double]) {
            $value2.ToString('R', [System.Globalization.CultureInfo]::InvariantCulture)
        } else {
            [string]$value2
        }
        $clean = { param($s) ([string]$s).Replace("`t", ' ').Replace("`r", ' ').Replace("`n", ' ') }
        $lines.Add((@($name, 'Cases', "C$r", $formula, (& $clean $text), $type, (& $clean $raw)) -join "`t"))
    }
    [System.IO.File]::WriteAllLines($tsv, $lines, (New-Object System.Text.UTF8Encoding($false)))
    Write-Output "Wrote $xlsx and $tsv ($($row - 2) cases)"
}
finally {
    if ($workbook) {
        $workbook.Close($false)
        [System.Runtime.InteropServices.Marshal]::ReleaseComObject($workbook) | Out-Null
    }
    if ($excel) {
        $excel.Quit()
        [System.Runtime.InteropServices.Marshal]::ReleaseComObject($excel) | Out-Null
    }
}
