# GUI drafts (ASCII)

Interactive version: [`gui-draft.html`](gui-draft.html) (open in a browser).

## Project window
```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ● ● ●  Customer API                                           [Staging ▾] [▶]           [≣]        │
├──────────────────────┬───────────────────────────────────────────┬─────────────────────────────────┤
│ REQUESTS             │ GetCustomer 1  SOAP 1.1 · GetCustomer  ✓  │ 200 · 143 ms · 1.2 KB · Staging │
│  ▾ GetCustomer/      │ ┌───┬───────────────────────────────────┐ │        [Response] [Headers]     │
│   ▸ GetCustomer 1    │ │  1│<soapenv:Envelope xmlns:soapenv=…  │ │ <soap:Envelope …>               │
│     GetCustomer 2  • │ │  2│                  xmlns:cus=…>     │ │   <soap:Body>                   │
│  ▾ CreateOrder/      │ │  3│  <soapenv:Header/>                │ │     <GetCustomerResponse>       │
│     CreateOrder 1  ⚠ │ │  4│  <soapenv:Body>                   │ │       <customer>…               │
│    Smoke test        │ │ ⚠5│    <cus:GetCustomer>              │ │                                 │
│                      │ │  6│      <cus:customerId>?</cus:cust… │ │                                 │
│                      │ │  …│                                   │ │                                 │
│                      │ └───┴───────────────────────────────────┘ ├─────────────────────────────────┤
│                      │ ⚠ 1 error  line 6: '?' is not a valid xs… │ ▸ History  6 earlier    ●●●●●●◉ │
└──────────────────────┴───────────────────────────────────────────┴─────────────────────────────────┘
   / folder   • unsaved   ⚠ fails validation   ● past exchange (red: fault or failure)   ◉ the one shown
```

An older history entry selected (drawer open):
```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ● ● ●  Customer API                                                                                │
│ Older exchange · Today 13:51:07 · Staging                  [Restore Request]  [[Show Latest]]      │
├──────────────────────┬───────────────────────────────────────────┬─────────────────────────────────┤
│ REQUESTS             │ GetCustomer 1   Sent 13:51:07 · read-only │ 500 · 88 ms · SOAP Fault: soap… │
│  ▸ GetCustomer 1     │ ┌───┬───────────────────────────────────┐ │        [Response] [Headers]     │
│    …                 │ │  1│<soapenv:Envelope …>   (read-only) │ │ <soap:Fault>…                   │
│                      │ │  …│      <cus:customerId>0</cus:cust… │ ├─────────────────────────────────┤
│                      │ │   │                                   │ │ ▾ History  6 earlier    ●●●●●◉● │
│                      │ │   │                                   │ │ Today 14:03:12  Staging  200    │
│                      │ │   │                                   │ │▸Today 13:51:07  Staging  500 F… │
│                      │ └───┴───────────────────────────────────┘ │ Today 11:20:44  Local    200    │
└──────────────────────┴───────────────────────────────────────────┴─────────────────────────────────┘
```

## New project window
```
┌ New Project ─────────────────────────────────────────────┐
│ Name:      [Customer API                          ]       │
│ Location:  ~/Projects/soap               [Choose…]        │
│ WSDL:      CustomerService.wsdl          [Choose…]        │
│ XSD files: common/types.xsd, faults.xsd  [Add…] [−]       │
│                                                           │
│ References                                                │
│  ✓ xs:import urn:example:common → common/types.xsd        │
│  ✓ xs:import urn:example:faults → faults.xsd              │
│  ✗ xs:include addresses.xsd    — not supplied             │
│                                                           │
│                               [Cancel]  [Create] (disabled)│
└───────────────────────────────────────────────────────────┘
```

## Settings window → a project's Servers
```
┌ Servers ────────────────────────────────────────────────────────┐
│ WASHBOARD          │ These settings belong to Customer API      │
│  ⚙ General         │ and are saved in its folder.               │
│ CUSTOMER API       │ Servers                                    │
│  ▤ General         │ ╭────────────────────────────────────────╮ │
│ ▸▤ Servers         │ │ Production                     [Edit…] │ │
│                    │ │ https://api.example.com/ws             │ │
│                    │ │ Staging                        [Edit…] │ │
│                    │ │ https://stg.example.com/ws             │ │
│                    │ │ CustomerPort From WSDL [Add as Server…]│ │
│                    │ │ https://api.example.com/ws             │ │
│                    │ │                         [Add Server…]  │ │
│                    │ ╰────────────────────────────────────────╯ │
└─────────────────────────────────────────────────────────────────┘

Edit… (sheet)
┌──────────────────────────────────────────────┐
│ Staging                                      │
│ ╭──────────────────────────────────────────╮ │
│ │ Name                 [Staging          ] │ │
│ │ URL     [https://stg.example.com/ws    ] │ │
│ │ Ignore certificate errors           [ ○] │ │
│ │ Authentication          (•) None ( ) Basic│ │
│ │ Timeout                       [60] seconds│ │
│ ╰──────────────────────────────────────────╯ │
│ [Delete Server]            [Cancel] [Save]   │
└──────────────────────────────────────────────┘
```

## HTTP log panel
```
┌ HTTP Log ────────────────────────────────────────────────────────┐
│ 14:03:12  Customer API  POST stg.example.com  200  143 ms  ← last │
│ 14:01:55  Customer API  POST stg.example.com  500  98 ms          │
├──────────────────────────────┬───────────────────────────────────┤
│ POST /ws/customer HTTP/1.1   │ HTTP/1.1 200 OK                   │
│ Host: stg.example.com        │ Content-Type: text/xml; charset=… │
│ Content-Type: text/xml; …    │ Content-Length: 1234              │
│ SOAPAction: "urn:…/GetCust…" │                                   │
│ Authorization: Basic ••••••  │ <soap:Envelope …>                 │
│                              │                                   │
│ <soapenv:Envelope …>         │                                   │
│ TLS · certificate verificat… │                                   │
└──────────────────────────────┴───────────────────────────────────┘
```
Authorization values are masked in the log by default (click to reveal).
