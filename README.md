# washboard quickstart

```shell
cargo build
cp ./target/debug/washboard ~/bin/
washboard project new --wsdl myservice.wsdl ~/myservice
cd ~/myservice
washboard server add reference http://192.0.2.1:8080/myservice
washboard server add production https://192.0.2.2/myservice
washboard request new querySubscription --name query1
vim requests/query1.xml  # do whatever
washboard request send --skip-validation --server reference query1
```

## send example

```
washboard request send --skip-validation --server reference query1
note: sending without validation
HTTP/1.1 200 OK  221 ms  494 bytes
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
        <soapenv:Body>
                <querySubscriptionResponse xmlns="http://example.org/myservice/">
                        <subscriber>AAAA1</subscriber>
                        <validUntil>2098-12-31T23:59:59</validUntil>
                </querySubscriptionResponse>
        </soapenv:Body>
</soapenv:Envelope>
```

## history example

```
washboard request history query1 --show 1
--- request sent to https://192.0.2.2/myservice
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"
                  xmlns="http://example.org/myservice/"
                  xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <soapenv:Body>
    <querySubscriptionRequest>
      <subscriber>AAAA1</subscriber>
    </querySubscriptionRequest>
  </soapenv:Body>
</soapenv:Envelope>

--- response
date: Wed, 07 Oct 2026 15:26:05 GMT
content-type: application/soap+xml; charset=ISO-8859-1
content-length: 494
server: Jetty

<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
        <soapenv:Body>
                <querySubscriptionResponse xmlns="http://example.org/myservice/">
                        <subscriber>AAAA1</subscriber>
                        <validUntil>2098-12-31T23:59:59</validUntil>
                </querySubscriptionResponse>
        </soapenv:Body>
</soapenv:Envelope>
```

