"""Response contract checks against real native web objects and Pyodide FFI."""
from http import HTTPStatus
import json
import js

from workers import Response, WorkerEntrypoint


class Default(WorkerEntrypoint):
    async def fetch(self, request):
        payload = {'large':9007199254740993, 'unicode':'λ', 'nested':[None, True, {'x':2}]}
        first = Response.from_json(payload)
        second = Response.from_json(payload)
        assert first.status == 200
        assert first.headers['content-type'] == 'application/json'
        first.js_object.headers.set('x-isolated', 'yes')
        assert not second.js_object.headers.has('x-isolated')
        assert first.url == ''
        assert await first.text() == json.dumps(payload)
        assert first.body_used
        try:
            await first.text()
        except OSError:
            pass
        else:
            raise AssertionError('consumed response body was readable twice')
        assert await second.json() == payload
        custom = Response.from_json(payload, status=HTTPStatus.CREATED, status_text='Created', headers={'content-type':'application/custom', 'x-test':'yes'})
        assert custom.status == 201
        assert custom.status_text == 'Created'
        assert custom.headers['content-type'] == 'application/custom'
        assert custom.headers['x-test'] == 'yes'
        assert await custom.json() == payload
        native = Response.from_json(js.JSON.parse('{"native":true}'))
        assert await native.json() == {'native':True}
        wrapped = Response(js.Response.new('native body', status=202))
        assert wrapped.status == 202
        assert await wrapped.text() == 'native body'
        try:
            Response.from_json({'bad':object()})
        except TypeError:
            pass
        else:
            raise AssertionError('non-JSON Python value accepted')
        return Response.from_json({'responses':'ok'})
