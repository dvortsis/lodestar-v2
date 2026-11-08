import importlib.util
import os
import sys
import unittest
from datetime import datetime, timedelta, timezone
from unittest.mock import Mock, patch

# BEGIN ISOLATED AWS SDK TEST STUBS
# Stub AWS SDK imports exclusively while importing the Lambda under test.
# These tests never contact AWS; production Lambda code is not changed.
import types

_boto3 = types.ModuleType('boto3')
_boto3.client = lambda service, **kwargs: Mock()
_botocore = types.ModuleType('botocore')
_exceptions = types.ModuleType('botocore.exceptions')
_exceptions.ClientError = type('ClientError', (Exception,), {})
_botocore.exceptions = _exceptions
with patch.dict(sys.modules, {
    'boto3': _boto3,
    'botocore': _botocore,
    'botocore.exceptions': _exceptions,
}):
# END ISOLATED AWS SDK TEST STUBS
    spec = importlib.util.spec_from_file_location('watchdog', os.path.join(os.path.dirname(__file__), 'index.py'))
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)

class WatchdogTests(unittest.TestCase):
    def setUp(self):
        self.now = datetime.now(timezone.utc)
        self.created = self.now-timedelta(hours=49)
        os.environ['MIN_CREATED_UTC'] = (self.now-timedelta(days=4)).isoformat().replace('+00:00','Z')
        os.environ['ALERT_TOPIC_ARN'] = 'arn:aws:sns:us-east-1:123456789012:topic'
        self.vm = {'id':12345,'label':'lodestar-worker-01','tags':['lodestar','test'],'region':'us-east','type':'g6-nanode-1','created':self.created.isoformat().replace('+00:00','Z')}

    def test_approved_scope(self):
        self.assertTrue(mod.scoped(self.vm))
        for changed in [ {'label':'other-project'}, {'region':'us-west'}, {'type':'g6-standard-8'}, {'tags':['lodestar']}, {'id':'12345'} ]:
            self.assertFalse(mod.scoped({**self.vm,**changed}), changed)

    def test_48h_boundary(self):
        self.assertTrue(mod.aged(self.vm,self.now))
        future={**self.vm,'created':(self.now-timedelta(hours=47)).isoformat().replace('+00:00','Z')}
        self.assertFalse(mod.aged(future,self.now))

    def test_pre_activation_excluded(self):
        os.environ['MIN_CREATED_UTC']=(self.now-timedelta(hours=30)).isoformat().replace('+00:00','Z')
        self.assertFalse(mod.aged(self.vm,self.now))

    @patch.object(mod,'token_value', return_value=None)
    @patch.object(mod,'list_paginated')
    def test_missing_token_no_external_api(self,list_fn,_token):
        result=mod.handler({},None)
        self.assertEqual(result['status'],'unconfigured')
        list_fn.assert_not_called()

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_dry_run_no_delete(self,api,list_fn,_token):
        list_fn.return_value=[self.vm]
        result=mod.handler({'dry_run':True},None)
        self.assertEqual(result['eligible'],1)
        self.assertEqual(result['deleted'],0)
        api.assert_not_called()

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    @patch.object(mod,'confirm_deleted',return_value={12345})
    def test_delete_only_eligible(self,confirm,api,list_fn,_token):
        list_fn.return_value=[self.vm]
        api.side_effect=[self.vm,{}]
        result=mod.handler({},None)
        self.assertEqual(result['deleted'],1)
        self.assertEqual(api.call_args_list[0].args[0],'GET')
        self.assertEqual(api.call_args_list[1].args[0],'DELETE')
        confirm.assert_called_once()

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_scope_change_blocks_deletion(self,api,list_fn,_token):
        list_fn.return_value=[self.vm]
        api.return_value={**self.vm,'tags':['lodestar']}
        result=mod.handler({},None)
        self.assertEqual(result['deleted'],0)
        self.assertEqual(len(api.call_args_list),1)

    def test_api_path_allowlist(self):
        # Only instance paths are reachable; the guard rejects anything else
        # before a request is built, so no network call happens here.
        for bad in ['linode/instances', '//linode/instances', '/linode/../account',
                    '/networking/firewalls?page=1&page_size=100', '/account?x=1']:
            with self.assertRaises(ValueError, msg=bad):
                mod.api('GET', bad, 'thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')

    def tearDown(self):
        os.environ.pop('TOKEN_REQUIRED', None)

    def _vms(self, n):
        return [{**self.vm, 'id': 1000 + i, 'label': 'lodestar-worker-%02d' % i} for i in range(n)]

    @patch.object(mod,'token_value', return_value=None)
    @patch.object(mod,'list_paginated')
    def test_missing_token_raises_when_required(self,list_fn,_token):
        os.environ['TOKEN_REQUIRED']='true'
        with self.assertRaises(RuntimeError):
            mod.handler({},None)
        list_fn.assert_not_called()

    def test_future_created_excluded(self):
        skewed={**self.vm,'created':(self.now+timedelta(minutes=30)).isoformat().replace('+00:00','Z')}
        self.assertFalse(mod.aged(skewed,self.now))

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_more_than_ten_eligible_stops_everything(self,api,list_fn,_token):
        list_fn.return_value=self._vms(11)
        with self.assertRaises(RuntimeError):
            mod.handler({},None)
        api.assert_not_called()

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    @patch.object(mod,'confirm_deleted',side_effect=lambda ids,token,deadline_s=None: set(ids))
    def test_at_most_four_deletions_per_run(self,_confirm,api,list_fn,_token):
        vms=self._vms(6)
        list_fn.return_value=vms
        by_id={v['id']:v for v in vms}
        api.side_effect=lambda method,path,token,allow_404=False: by_id[int(path.rsplit('/',1)[1])] if method=='GET' else {}
        result=mod.handler({},None)
        self.assertEqual(result['eligible'],6)
        self.assertEqual(result['deleted'],4)
        self.assertEqual(sum(1 for c in api.call_args_list if c.args[0]=='DELETE'),4)

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    @patch.object(mod,'confirm_deleted',return_value=set())
    def test_unverified_deletion_raises_without_alert(self,_confirm,api,list_fn,_token):
        list_fn.return_value=[self.vm]
        api.side_effect=[self.vm,{}]
        with patch.object(mod,'SNS') as sns:
            with self.assertRaises(RuntimeError):
                mod.handler({},None)
            sns.publish.assert_not_called()

    def _page(self, page, pages, n=1):
        return {'data':[{'id':i} for i in range(n)],'page':page,'pages':pages}

    @patch.object(mod,'api')
    def test_pagination_reads_all_pages(self,api):
        api.side_effect=[self._page(1,2),self._page(2,2)]
        self.assertEqual(len(mod.list_paginated('/linode/instances','t')),2)

    @patch.object(mod,'api')
    def test_pagination_rejects_bad_responses(self,api):
        for bad in [self._page(2,2), self._page(1,21), self._page(1,0), {'data':None,'page':1,'pages':1},
                    self._page(1,1,n=501)]:
            api.side_effect=[bad]
            with self.assertRaises(RuntimeError, msg=bad.get('pages')):
                mod.list_paginated('/linode/instances','t')

    def _secret(self, value):
        client=Mock()
        client.get_secret_value.return_value={'SecretString':value}
        return patch.object(mod,'SECRETS',client)

    @patch.dict(os.environ, {'SECRET_ARN': 'arn:aws:secretsmanager:us-east-1:123456789012:secret:x'})
    def test_token_value_parsing(self):
        good='a'*40
        with self._secret(good):
            self.assertEqual(mod.token_value(),good)
        with self._secret('{"token": "%s"}' % good):
            self.assertEqual(mod.token_value(),good)
        with self._secret(''):
            self.assertIsNone(mod.token_value())
        for bad in ['short', 'a'*20+' '+'a'*20, 'a'*513, '{"other": "x"}']:
            with self._secret(bad):
                with self.assertRaises(RuntimeError, msg=bad[:10]):
                    mod.token_value()

    def test_linode_zoneless_created_format(self):
        # The Linode API returns created as "2018-01-01T00:01:01" with no zone; it is UTC.
        zoneless={**self.vm,'created':self.created.strftime('%Y-%m-%dT%H:%M:%S')}
        self.assertTrue(mod.aged(zoneless,self.now))
        young={**self.vm,'created':(self.now-timedelta(hours=47)).strftime('%Y-%m-%dT%H:%M:%S')}
        self.assertFalse(mod.aged(young,self.now))

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_lodestar_tagged_out_of_scope_is_logged_as_error(self,api,list_fn,_token):
        stray=[{**self.vm,'id':1,'region':'us-west'}, {**self.vm,'id':2,'type':'g6-standard-8'},
               {**self.vm,'id':3,'label':'other'}, {**self.vm,'id':4,'tags':['lodestar']}]
        unrelated={**self.vm,'id':5,'tags':['other'],'region':'us-west'}
        list_fn.return_value=stray+[unrelated]
        with self.assertLogs(mod.LOG, level='ERROR') as logs:
            result=mod.handler({},None)
        self.assertEqual(result['out_of_scope'],4)
        self.assertEqual(sum('"out_of_scope_linode"' in line for line in logs.output),4)
        self.assertEqual(result['eligible'],0)
        api.assert_not_called()

    @patch.object(mod,'api')
    def test_confirm_deleted_waits_on_a_shared_deadline(self,api):
        clock={'t':0.0}
        sleeps=[]
        def fake_sleep(s):
            sleeps.append(s); clock['t']+=s
        # id 1 is gone at once; id 2 only after 30 s; id 3 never.
        def fake_api(method,path,token,allow_404=False):
            id_=int(path.rsplit('/',1)[1])
            if id_==1 or (id_==2 and clock['t']>=30):
                return None
            return {'id':id_}
        api.side_effect=fake_api
        with patch.object(mod.time,'monotonic',lambda: clock['t']), patch.object(mod.time,'sleep',fake_sleep):
            confirmed=mod.confirm_deleted([1,2,3],'t')
        self.assertEqual(confirmed,{1,2})
        self.assertGreaterEqual(clock['t'],mod.CONFIRM_DEADLINE_S)
        self.assertGreaterEqual(mod.CONFIRM_DEADLINE_S,60)
        self.assertTrue(all(s==mod.CONFIRM_POLL_S for s in sleeps))

    @patch.object(mod,'api')
    def test_confirm_deleted_returns_early_when_all_gone(self,api):
        api.return_value=None
        with patch.object(mod.time,'sleep') as sleep:
            self.assertEqual(mod.confirm_deleted([1,2],'t'),{1,2})
        sleep.assert_not_called()

    def _two_vms_api(self, api):
        vms=self._vms(2)
        by_id={v['id']:v for v in vms}
        api.side_effect=lambda method,path,token,allow_404=False: by_id[int(path.rsplit('/',1)[1])] if method=='GET' else {}
        return vms

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_partial_confirmation_alerts_confirmed_and_fails_on_the_rest(self,api,list_fn,_token):
        vms=self._two_vms_api(api); list_fn.return_value=vms
        with patch.object(mod,'confirm_deleted',return_value={1000}), patch.object(mod,'SNS') as sns:
            with self.assertRaises(RuntimeError) as err:
                mod.handler({},None)
        sns.publish.assert_called_once()
        self.assertIn('[1000]', sns.publish.call_args.kwargs['Message'])
        self.assertNotIn('1001', sns.publish.call_args.kwargs['Message'])
        self.assertIn('1001', str(err.exception))
        self.assertIn('DeletionNotVerified', str(err.exception))

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_confirmation_exception_fails_without_alert(self,api,list_fn,_token):
        vms=self._two_vms_api(api); list_fn.return_value=vms
        with patch.object(mod,'confirm_deleted',side_effect=RuntimeError('boom')), patch.object(mod,'SNS') as sns:
            with self.assertRaises(RuntimeError):
                mod.handler({},None)
        sns.publish.assert_not_called()

    @patch.object(mod,'api')
    def test_failed_status_check_keeps_earlier_confirmations(self,api):
        clock={'t':0.0}
        def fake_sleep(s): clock['t']+=s
        def fake_api(method,path,token,allow_404=False):
            id_=int(path.rsplit('/',1)[1])
            if id_==1: return None
            raise RuntimeError('Akamai API HTTP status 503')
        api.side_effect=fake_api
        with patch.object(mod.time,'monotonic',lambda: clock['t']), patch.object(mod.time,'sleep',fake_sleep):
            self.assertEqual(mod.confirm_deleted([1,2],'t',deadline_s=20),{1})

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_confirmation_budget_respects_lambda_time_left(self,api,list_fn,_token):
        vms=self._two_vms_api(api); list_fn.return_value=vms
        ctx=Mock(); ctx.get_remaining_time_in_millis.return_value=70_000
        with patch.object(mod,'confirm_deleted',side_effect=lambda ids,token,deadline_s: set(ids)) as confirm, \
                patch.object(mod,'SNS'):
            mod.handler({},ctx)
        self.assertEqual(confirm.call_args.kwargs['deadline_s'],70-mod.REPORT_RESERVE_S)

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_low_time_defers_deletions_and_fails(self,api,list_fn,_token):
        vms=self._two_vms_api(api); list_fn.return_value=vms
        ctx=Mock(); ctx.get_remaining_time_in_millis.return_value=30_000
        with self.assertRaises(RuntimeError) as err:
            mod.handler({},ctx)
        self.assertIn('DeferredLowTime', str(err.exception))
        self.assertFalse(any(c.args[0]=='DELETE' for c in api.call_args_list))

if __name__=='__main__': unittest.main(verbosity=2)
