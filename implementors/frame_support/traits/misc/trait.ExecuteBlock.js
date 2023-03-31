(function() {var implementors = {
"domain_pallet_executive":[["impl&lt;System:&nbsp;Config + EnsureInherentsAreFirst&lt;Block&gt;, Block:&nbsp;Block&lt;Header = System::Header, Hash = System::Hash&gt;, Context:&nbsp;<a class=\"trait\" href=\"https://doc.rust-lang.org/nightly/core/default/trait.Default.html\" title=\"trait core::default::Default\">Default</a>, UnsignedValidator, AllPalletsWithSystem:&nbsp;OnRuntimeUpgrade + OnInitialize&lt;System::BlockNumber&gt; + OnIdle&lt;System::BlockNumber&gt; + OnFinalize&lt;System::BlockNumber&gt; + OffchainWorker&lt;System::BlockNumber&gt;, ExecutiveConfig:&nbsp;<a class=\"trait\" href=\"domain_pallet_executive/trait.Config.html\" title=\"trait domain_pallet_executive::Config\">Config</a>, COnRuntimeUpgrade:&nbsp;OnRuntimeUpgrade&gt; ExecuteBlock&lt;Block&gt; for <a class=\"struct\" href=\"domain_pallet_executive/struct.Executive.html\" title=\"struct domain_pallet_executive::Executive\">Executive</a>&lt;System, Block, Context, UnsignedValidator, AllPalletsWithSystem, ExecutiveConfig, COnRuntimeUpgrade&gt;<span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;Block::Extrinsic: Checkable&lt;Context&gt; + Codec,<br>&nbsp;&nbsp;&nbsp;&nbsp;<a class=\"type\" href=\"domain_pallet_executive/type.CheckedOf.html\" title=\"type domain_pallet_executive::CheckedOf\">CheckedOf</a>&lt;Block::Extrinsic, Context&gt;: Applyable + GetDispatchInfo,<br>&nbsp;&nbsp;&nbsp;&nbsp;<a class=\"type\" href=\"domain_pallet_executive/type.CallOf.html\" title=\"type domain_pallet_executive::CallOf\">CallOf</a>&lt;Block::Extrinsic, Context&gt;: Dispatchable&lt;Info = DispatchInfo, PostInfo = PostDispatchInfo&gt;,<br>&nbsp;&nbsp;&nbsp;&nbsp;<a class=\"type\" href=\"domain_pallet_executive/type.OriginOf.html\" title=\"type domain_pallet_executive::OriginOf\">OriginOf</a>&lt;Block::Extrinsic, Context&gt;: <a class=\"trait\" href=\"https://doc.rust-lang.org/nightly/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"enum\" href=\"https://doc.rust-lang.org/nightly/core/option/enum.Option.html\" title=\"enum core::option::Option\">Option</a>&lt;System::AccountId&gt;&gt;,<br>&nbsp;&nbsp;&nbsp;&nbsp;UnsignedValidator: ValidateUnsigned&lt;Call = <a class=\"type\" href=\"domain_pallet_executive/type.CallOf.html\" title=\"type domain_pallet_executive::CallOf\">CallOf</a>&lt;Block::Extrinsic, Context&gt;&gt;,</span>"]],
"substrate_test_runtime":[["impl ExecuteBlock&lt;Block&lt;Header&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.u64.html\">u64</a>, BlakeTwo256&gt;, <a class=\"enum\" href=\"substrate_test_runtime/enum.Extrinsic.html\" title=\"enum substrate_test_runtime::Extrinsic\">Extrinsic</a>&gt;&gt; for <a class=\"struct\" href=\"substrate_test_runtime/system/struct.BlockExecutor.html\" title=\"struct substrate_test_runtime::system::BlockExecutor\">BlockExecutor</a>"]]
};if (window.register_implementors) {window.register_implementors(implementors);} else {window.pending_implementors = implementors;}})()